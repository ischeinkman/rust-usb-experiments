extern crate libusb;

extern crate scsi;
use scsi::{ScsiError, ErrorCause};

extern crate mbr_nostd;
use mbr_nostd::PartitionTable;
use mbr_nostd::PartitionTableEntry;

extern crate fatfs;

mod err;
use err::*;

use libusb::{Context, Device, DeviceDescriptor, DeviceHandle, TransferType};
use std::io::stdin;
use std::string::String;
use std::time::Duration;
use std::io::{Write, Read, Seek, SeekFrom, BufRead};

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use std::time::Instant;


fn main() {
    usb_test();
}

fn usb_test()  {
    let mut usb_ctx: libusb::Context = libusb::Context::new().unwrap();
    let device_list = usb_ctx.devices().unwrap();
    let mut device_iter = device_list.iter();
    let wrapper = loop {
        let mut try_device: Device = device_iter.next().ok_or("Ran out of devices!").unwrap();
        let device_desc: libusb::DeviceDescriptor = try_device.device_descriptor().unwrap();
        println!(
            "Found device with VendorID: {:x}, ProductID {:x}. Connect?",
            device_desc.vendor_id(),
            device_desc.product_id()
        );
        let mut response = String::new();
        stdin().read_line(&mut response).unwrap();
        if !response.to_lowercase().starts_with('y') {
            continue;
        }
        break UsbClient::from_device(&mut try_device).unwrap();
    };

    let mut scsi_wrapper = scsi::scsi::ScsiBlockDevice::new(wrapper, VecNewtype::new(), VecNewtype::new(), VecNewtype::new()).unwrap();
    println!("SCSI_CSW: {}", fmt_o_csw(&scsi_wrapper.prev_csw));
    println!("Block size : {}", scsi_wrapper.block_size());
    std::thread::sleep(Duration::from_secs(3));
    let mut mbr_buff = VecNewtype::with_fake_capacity(scsi_wrapper.block_size() as usize);
    println!("Trying to get MBR.");
    while mbr_buff.inner.len() < 512 {
        use scsi::Buffer;
        println!("MBR Buffer stats: size = {}, capacity = {}, inner.len() = {}", mbr_buff.size(), mbr_buff.capacity(), mbr_buff.inner.len());
        let bt = scsi_wrapper.read(mbr_buff.inner.len() as u32, &mut mbr_buff).unwrap();
        println!("SCSI_CSW: {}", fmt_o_csw(&scsi_wrapper.prev_csw));
        println!("Got {} more mbr bytes! Now have {}.", bt, mbr_buff.inner.len());
    }
    println!("Finished getting MBR.");
    let mbr_entry = mbr_nostd::MasterBootRecord::from_bytes(&mut mbr_buff.inner).unwrap();
    for ent in mbr_entry.partition_table_entries() {
        println!("{:?}", ent)
    }
    let first_ent : &PartitionTableEntry = &mbr_entry.partition_table_entries()[0];
    let raw_offset : usize = (first_ent.logical_block_address * scsi_wrapper.block_size()) as usize; 

    let mut partition = OffsetScsiDevice::new(scsi_wrapper, raw_offset);

    let mut fs : fatfs::FileSystem<OffsetScsiDevice> = fatfs::FileSystem::new(partition, fatfs::FsOptions::new()).unwrap();
    {
        let mut root_dir = fs.root_dir();
        let subdir_opt = root_dir.iter().find_map(|ent_res| {
            let fl = ent_res.unwrap();
            println!("FAT: Found itm {}", fl.file_name());
            if fl.is_dir() && fl.file_name() == "test_folder".to_owned() {
                Some(fl)
            }
            else {
                None
            }
        });
        let mut subdir = match subdir_opt {
            Some(fl) => fl.to_dir(),
            None => {
                root_dir.create_dir("test_folder").unwrap()
            }
        };

        let now = Instant::now();
        let fl_name = format!("test_t_2018-12-10.txt");
        let mut fl = subdir.create_file(&fl_name).unwrap();

        fl.write_fmt(format_args!("Hello world at time {:?}", now)).unwrap();
        fl.flush().unwrap();
        let mut next_dir = root_dir.create_dir(format!("{:?}.txt", now).replace(" ", "__").replace(":", "..").replace("{", "(").replace("}", ")").as_str()).unwrap();
        let mut outfile = next_dir.create_file("for_seuth.txt").unwrap();
        outfile.write("To be or not to be and all that jazz!.".to_owned().into_bytes().as_slice()).unwrap();
        outfile.flush().unwrap();
        println!("Status: {:?}", fs.read_status_flags().unwrap());
    }
    fs.unmount().unwrap();
}

#[derive(Debug, Copy, Clone)]
struct Endpoint {
    config: u8,
    iface: u8,
    setting: u8,
    address: u8,
}

#[derive(Debug, Copy, Clone)]
pub struct ReadEndpoint(Endpoint);

#[derive(Debug, Copy, Clone)]
pub struct WriteEndpoint(Endpoint);

pub struct UsbClient<'a> {
    device_handle: DeviceHandle<'a>,
    read_endpoint: ReadEndpoint,
    write_endpoint: WriteEndpoint,
}

impl<'a> UsbClient<'a> {
    pub fn from_device(device: &mut Device<'a>) -> Result<UsbClient<'a>, RawStringErr> {
        let desc = device.device_descriptor().unwrap();
        let (rd, wd) = UsbClient::find_bulk_endpoints(device, &desc).unwrap();
        let mut hndl = device.open().map_err(|e| format!("Open err: {:?}", e)).unwrap();
        if hndl.kernel_driver_active(rd.0.iface).map_err(|e| format!("Error checking kernel: {:?}", e)).unwrap() {
            hndl.detach_kernel_driver(rd.0.iface).map_err(|e| format!("Found kernel detach err: {:?}", e)).unwrap();
        };
        hndl.reset().map_err(|e| format!("Found reset err: {:?}", e)).unwrap();
        hndl.set_active_configuration(rd.0.config).map_err(|e| format!("Could not set active config: {:?}", e)).unwrap();
        hndl.claim_interface(rd.0.iface).map_err(|e| format!("Could not claim iface {}: {:?}", rd.0.iface, e)).unwrap();
        Ok(UsbClient::new(hndl, rd, wd))
    }
    pub fn new(
        device_handle: DeviceHandle<'a>,
        read_endpoint: ReadEndpoint,
        write_endpoint: WriteEndpoint,
    ) -> UsbClient<'a> {
        UsbClient {
            device_handle,
            read_endpoint,
            write_endpoint,
        }
    }

    pub fn pull_bytes(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        println!("USBC: Trying to pull {} bytes.", buffer.len());
        let endpoint = self.read_endpoint.0;
        let timeout = Duration::from_secs(30);
        let rval = self
            .device_handle
            .read_bulk(endpoint.address, buffer, timeout)
            .map_err(|e| format!("Read Error: {:?}", e)).unwrap();
        println!("USBC: Pulled bytes: [{}]", buffer[0 .. rval].iter().map(|bt| format!("0x{:x}, ", bt)).collect::<String>());
        Ok(rval)
    }

    pub fn push_bytes(&mut self, buffer: &[u8]) -> Result<usize, String> {
        println!("USBC: Trying to push bytes [{}]", buffer.iter().map(|bt| format!("0x{:x}, ", bt)).collect::<String>());
        let endpoint = self.write_endpoint.0;
        let timeout = Duration::from_secs(30);
        let rval = self
            .device_handle
            .write_bulk(endpoint.address, buffer, timeout)
            .map_err(|e| format!("Write Error: {:?}", e)).unwrap();
        println!("USBC: Success: {}", rval);
        Ok(rval)
    }
    fn find_bulk_endpoints(
        device: &mut Device,
        desc: &DeviceDescriptor,
    ) -> Result<(ReadEndpoint, WriteEndpoint), RawStringErr> {

        let is_scsi_bulk_device =
            desc.class_code() == 8 && desc.sub_class_code() == 6 && desc.protocol_code() == 80;

        for config_idx in 0..desc.num_configurations() {
            let config_desc: libusb::ConfigDescriptor = match device.config_descriptor(config_idx) {
                Ok(c) => c,
                Err(_) => continue,
            };

            for interface in config_desc.interfaces() {
                for interface_desc in interface.descriptors() {
                    if !is_scsi_bulk_device && !(interface_desc.class_code() == 8
                        && interface_desc.sub_class_code() == 6
                        && interface_desc.protocol_code() == 80)
                    {
                        continue;
                    }
                    let mut endpoints: Vec<libusb::EndpointDescriptor> =
                        interface_desc.endpoint_descriptors().collect();
                    let endpoint_a = endpoints
                        .pop()
                        .ok_or(format!("Found no endpoints in interface!")).unwrap();
                    let endpoint_b = endpoints
                        .pop()
                        .ok_or(format!("Only found 1 endpoint in interface!")).unwrap();
                    let (read_desc, write_desc) = if endpoint_a.direction() == libusb::Direction::In
                        && endpoint_b.direction() == libusb::Direction::Out
                    {
                        (endpoint_a, endpoint_b)
                    } else if endpoint_a.direction() == libusb::Direction::Out
                        && endpoint_b.direction() == libusb::Direction::In
                    {
                        (endpoint_b, endpoint_a)
                    } else {
                        return Err(RawStringErr::from(format!(
                            "Both endpoints are in the same direction!"
                        )));
                    };
                    let read_endpoint = Endpoint {
                        config: config_desc.number(),
                        iface: interface_desc.interface_number(),
                        setting: interface_desc.setting_number(),
                        address: read_desc.address(),
                    };
                    let write_endpoint = Endpoint {
                        config: config_desc.number(),
                        iface: interface_desc.interface_number(),
                        setting: interface_desc.setting_number(),
                        address: write_desc.address(),
                    };

                    return Ok((ReadEndpoint(read_endpoint), WriteEndpoint(write_endpoint)));
                }
            }
        }
        Err(RawStringErr::from(
            "Could not find bulk read/write endpoints!",
        ))
    }
}

impl <'a> Drop for UsbClient<'a> {
    fn drop(&mut self) {
        //self.device_handle.attach_kernel_driver(self.read_endpoint.0.iface).unwrap();
    }
}

impl <'a> scsi::CommunicationChannel for UsbClient<'a> {
    fn in_transfer<B : scsi::Buffer> (&mut self, buffer: &mut B) -> Result<usize, scsi::ScsiError> {
        let mut shim = Vec::with_capacity(buffer.capacity());
        shim.resize(buffer.capacity(), 0);
        println!("CC: Starting in_transfer.");
        let rval = self.pull_bytes(shim.as_mut_slice())
        .map_err(|e|{ 
            eprintln!("Got error in read: {:?}", e);
            scsi::ScsiError::from_cause(scsi::ErrorCause::UsbTransferError{ direction: scsi::UsbTransferDirection::In})
        }).unwrap();
        println!("CC: Succeeded reading {} bytes.", rval);
        for byte in shim {
            buffer.push_byte(byte).unwrap();
        }
        Ok(rval)
    }

    fn out_transfer<B : scsi::Buffer>(&mut self, bytes: &mut B) -> Result<usize, scsi::ScsiError> {
        let mut shim = Vec::with_capacity(bytes.size());
        println!("CC: Starting out_transfer.");
        while bytes.size() > 0 {
            shim.push(bytes.pull_byte()?);
        }
        let rval = self.push_bytes(shim.as_ref())
        .map_err(|e|{ 
            eprintln!("Got error in write: {:?}", e);
            scsi::ScsiError::from_cause(scsi::ErrorCause::UsbTransferError{ direction: scsi::UsbTransferDirection::Out})
        }).unwrap();
        println!("CC: Succeeded writing {} bytes.", rval);
        Ok(rval)
    }
}

pub struct VecNewtype {
    inner : Vec<u8>, 
    fake_size : usize, 
}
impl VecNewtype {
    pub fn new() -> VecNewtype {
        VecNewtype::with_fake_capacity(512)
    }
    pub fn with_fake_capacity(sz : usize) -> VecNewtype {
        VecNewtype {
            inner : Vec::new(), 
            fake_size : sz,
        }
    }
}
impl From<Vec<u8>> for VecNewtype {
    fn from(inner : Vec<u8>) -> VecNewtype {
        let fake_size = if inner.len() > 512 {2 * inner.len()} else {512};
        VecNewtype {
            inner, 
            fake_size,
        }
    }
}
impl scsi::Buffer for VecNewtype {
    fn size(&self) -> usize {
        self.inner.len()
    }
    fn capacity(&self) -> usize {
        self.fake_size
    }
    fn push_byte(&mut self, byte : u8) -> Result<usize, scsi::ScsiError> {
        self.inner.push(byte);
        Ok(1)
    }
    fn pull_byte(&mut self) -> Result<u8, scsi::ScsiError> {
        if self.inner.is_empty() {
            Err(scsi::ScsiError::from_cause(scsi::ErrorCause::BufferTooSmallError{expected : 0, actual : 1}))
        }
        else {
            let bt = self.inner.remove(0);
            Ok(bt)
        }
    }
}   

pub struct OffsetScsiDevice<'a> {
    device : scsi::scsi::ScsiBlockDevice<UsbClient<'a>, VecNewtype, VecNewtype, VecNewtype>,
    block_buffer : VecNewtype,
    buffered_block_offset : usize,
    base_offset : usize,
    curr_index : usize,
    needs_flush : bool,

}
use std::io;
use scsi::Buffer;

impl <'a> Drop for OffsetScsiDevice<'a>{
    fn drop(&mut self) {
        if self.needs_flush {
            self.flush().unwrap();
        }
    }
}

impl <'a> OffsetScsiDevice<'a> {
    pub fn new(device : scsi::scsi::ScsiBlockDevice<UsbClient<'a>, VecNewtype, VecNewtype, VecNewtype>, base_offset : usize) -> Self {
        let block_size = device.block_size() as usize;
        OffsetScsiDevice {
            device, 
            block_buffer : VecNewtype::with_fake_capacity(block_size),
            buffered_block_offset : 0,
            base_offset,
            curr_index : 0,
            needs_flush : false,
        }
    }
}

impl <'a> BufRead for OffsetScsiDevice<'a> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.block_buffer.is_empty() {
            let offset = self.base_offset + self.buffered_block_offset;
            let red = self.device.read(offset as u32, &mut self.block_buffer)
                .map_err(|e| match e.cause {
                    scsi::ErrorCause::BufferTooSmallError {expected, actual} => {
                        io::Error::new(io::ErrorKind::UnexpectedEof, format!("Buffer too small: wanted {} but only have {}.", expected, actual))
                    },
                    e => io::Error::new(io::ErrorKind::Other, format!("Unmatched error : {:?}", e)),
                }).unwrap();
            println!("Offred: {}, {:?}", red, fmt_o_csw(&self.device.prev_csw));
        }
        let slice_offset = self.curr_index -  self.buffered_block_offset;
        Ok(&self.block_buffer.inner.as_slice()[slice_offset .. ])
    }

    fn consume(&mut self, amt: usize) {
        self.curr_index += amt;
        if self.curr_index > (self.buffered_block_offset + self.device.block_size() as usize) {
            if self.needs_flush {
                let _ = self.flush();
                self.needs_flush = false;
            }
            self.block_buffer.clear().unwrap();
            self.buffered_block_offset += self.device.block_size() as usize;
        }
    }
}

impl <'a> Read for OffsetScsiDevice<'a> {
    fn read(&mut self, buf : &mut [u8]) -> io::Result<usize> {
        let needed_bytes = buf.len();

        println!("std::Read: Requested {} bytes.", needed_bytes);

        let mut cur_idx = 0;
        while cur_idx < needed_bytes {
            let remaining_bytes = needed_bytes - cur_idx;
            println!("std::Read: have {} bytes remaining.", remaining_bytes);
            let to_consume = { 
                let cur_buff = &self.fill_buf().unwrap();
                let buflen = cur_buff.len();
                println!("std::Read: have {} bytes in the buffer.", buflen);
                if buflen == 0 {
                    println!("Reached end of device.");
                    return Ok(cur_idx);
                }
                else if buflen < remaining_bytes {
                    let mut out_slice = &mut buf[cur_idx .. cur_idx + buflen];
                    out_slice.copy_from_slice(&cur_buff[..]);
                    buflen
                }
                else {
                    let mut out_slice = &mut buf[cur_idx ..];
                    let in_slice = &cur_buff[ .. remaining_bytes];
                    out_slice.copy_from_slice(&in_slice);
                    remaining_bytes
                }
            };
            self.consume(to_consume);
            cur_idx += to_consume;
        }
        println!("std::Read: Finished reading {} bytes.", cur_idx);
        println!("std::Read: {:?}", buf);
        println!("std::Read: self.cur_idx = {}, self.buffered_block_offset = {}", self.curr_index, self.buffered_block_offset);
        return Ok(cur_idx);
    }
}

impl <'a> Write for OffsetScsiDevice<'a> {
    fn write(&mut self, bytes : &[u8]) -> io::Result<usize> {
        println!("std::Write: Writing {} bytes starting at {}.", bytes.len(), self.curr_index);
        let block_start_idx = self.curr_index - self.buffered_block_offset;
        let block_end_idx = bytes.len() + block_start_idx;
        let cur_block_size = self.device.block_size() as usize - block_start_idx;
        println!("Need write from {} to {} of size {}.", block_start_idx, block_end_idx, cur_block_size);
        {
            println!("std::Write: Checking and refreshing buffer.");
            if self.block_buffer.is_empty() {
                self.fill_buf()?;
            }
        }
        println!("std::Write: Before bytes: {:?}", self.block_buffer.inner);
        println!("std::Write: To write: {:?}", bytes);
        if bytes.len() > cur_block_size {
            println!("std::Write: Need more bytes than 1 block. Reading 2.");
            {
                let cur_block_part = &bytes[0 .. cur_block_size];
                let to_write_slice = &mut self.block_buffer.inner[block_start_idx .. block_end_idx];
                to_write_slice.copy_from_slice(cur_block_part);
                self.needs_flush = true;
            }
            {
                self.flush().unwrap();
                let next_block = self.fill_buf().unwrap();
                if next_block.len() < block_end_idx - cur_block_size {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
            }
            {
                let next_block_part = &bytes[cur_block_size ..];
                let to_write_slice = &mut self.block_buffer.inner[0 .. block_end_idx - cur_block_size];
                to_write_slice.copy_from_slice(next_block_part);
                self.needs_flush = true;
            }
        }
        else {
            println!("std::Write: Don't need more bytes than 1 block. Reading 1.");
            let cur_block_part = &bytes[..];
            let to_write_slice = &mut self.block_buffer.inner[block_start_idx .. block_end_idx];
            to_write_slice.copy_from_slice(cur_block_part);
            self.needs_flush = true;
        }
        println!("std::Write: After bytes:  {:?}", self.block_buffer.inner);
        self.consume(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        println!("std::Flush: Entered flush.");
        if !self.needs_flush {
            println!("std::Flush: Not doing flush.");
            return Ok(());
        }
        println!("std::Flush: Doing flush.");
        let offset_to_write = self.base_offset + self.buffered_block_offset;
        println!("std::Flush: Raw writing {}, {}.", offset_to_write, self.block_buffer.size());
        let _ = self.device.write(offset_to_write as u32, &mut self.block_buffer).unwrap();
        self.needs_flush = false;
        Ok(())
    }
}
impl <'a> Seek for OffsetScsiDevice<'a> {
    fn seek(&mut self, pos : SeekFrom) -> io::Result<u64> {
        let blk_size = self.device.block_size() as usize;
        match pos {
            SeekFrom::Start(absr) => {
                let abs = absr as usize;
                println!("std::Seek: Seeking to abs: {}", abs);

                let offset_from_block = abs % blk_size;
                let block_offset = abs - offset_from_block;
                if self.needs_flush {
                    self.flush().unwrap();
                }
                if block_offset != self.buffered_block_offset {
                    self.buffered_block_offset = block_offset;
                }
                let _ = self.block_buffer.clear();
                self.curr_index = abs;
                println!("std::Seek: idx now at {}", self.curr_index);
                Ok(abs as u64)
            },
            SeekFrom::Current(off) => {
                println!("std::Seek: Seeking to rel: {}", off);
                let abs = if off < 0 {
                    self.curr_index - off.abs() as usize
                } else { self.curr_index + off as usize };
                if self.needs_flush {
                    self.flush().unwrap();
                }

                let offset_from_block = abs % blk_size;
                let block_offset = abs - offset_from_block;
                if block_offset != self.buffered_block_offset {
                    self.buffered_block_offset = block_offset;
                }
                let _ = self.block_buffer.clear();
                self.curr_index = abs;
                println!("std::Seek: idx now at {}", self.curr_index);
                Ok(abs as u64)
            },
            _ => unimplemented!()
        }
    }
}

fn fmt_o_csw(csw: &Option<scsi::scsi::commands::CommandStatusWrapper>) -> String {
    match csw {
        None => "None".to_owned(),
        Some(csw) => {
            format!("Some({{ tag: {}, data_residue: {}, status: {} }})", csw.tag, csw.data_residue, csw.status)
        }
    }
}