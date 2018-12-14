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

mod usb_comm;
use usb_comm::*;

mod buf_scsi;
use buf_scsi::*;

fn main() {
    //rws_test();
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
    println!("Creating reader starting at offset block {}, raw {}.", first_ent.logical_block_address, raw_offset);

    let mut partition = OffsetScsiDevice::new(scsi_wrapper, raw_offset);

    let mut fs : fatfs::FileSystem<OffsetScsiDevice> = fatfs::FileSystem::new(partition, fatfs::FsOptions::new()).unwrap();
    println!("FAT: Have fs. Name from BPB: {:?}. Name from root dir: {:?}. Status: {:?}. Stats: {:?}", fs.volume_label(), fs.read_volume_label_from_root_dir().unwrap(), fs.read_status_flags().unwrap(), fs.stats().unwrap());
    {
        let mut root_dir = fs.root_dir();
        let subdir_opt = root_dir.iter().find_map(|ent_res| {
            let fl = ent_res.unwrap();
            println!("FAT: Found itm. Short name: {}, long name: {}, attr: {:?}", fl.short_file_name(), fl.file_name(), fl.attributes());
            if fl.is_dir() && fl.file_name() == "test_folder".to_owned() {
                Some(fl)
            }
            else {
                None
            }
        });
        let mut subdir = match subdir_opt {
            Some(fl) => {
                println!("FAT: Using existing subdir: Short name: {}, long name: {}, attr: {:?}", fl.short_file_name(), fl.file_name(), fl.attributes());
                fl.to_dir()
            },
            None => {
                root_dir.create_dir("test_folder").unwrap()
            }
        };

        let now = Instant::now();
        let fl_name = format!("test_t_2018-12-10.txt");
        let mut fl = subdir.create_file(&fl_name).unwrap();
        println!("FAT: Created fl {:?}", fl_name);

        fl.write_fmt(format_args!("Hello world at time {:?}", now)).unwrap();
        let next_dir_name = format!("{:?}.txt", now).replace(" ", "s").replace(":", "o").replace("{", "q").replace("}", "p");
        println!("FAT: Creating dir {}.", next_dir_name);
        let mut next_dir = root_dir.create_dir(&next_dir_name).unwrap();
        let mut outfile = next_dir.create_file("for_seuth.txt").unwrap();
        outfile.write("To be or not to be and all that jazz!.".to_owned().into_bytes().as_slice()).unwrap();
        println!("FAT: Ending with fs. Name from BPB: {:?}. Name from root dir: {:?}. Status: {:?}. Stats: {:?}", fs.volume_label(), fs.read_volume_label_from_root_dir().unwrap(), fs.read_status_flags().unwrap(), fs.stats().unwrap());
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


fn fmt_o_csw(csw: &Option<scsi::scsi::commands::CommandStatusWrapper>) -> String {
    match csw {
        None => "None".to_owned(),
        Some(csw) => {
            format!("Some({{ tag: {}, data_residue: {}, status: {} }})", csw.tag, csw.data_residue, csw.status)
        }
    }
}

fn rws_test() {
    const fl_name : &'static str = "a.txt";
    {
        let mut fl = File::create(fl_name).unwrap();
        fl.write_fmt(format_args!("abcdefg")).unwrap();
        fl.sync_all().unwrap();
    }
    {
        let mut fl_a = OpenOptions::new().read(true).write(true).open(fl_name).unwrap();
        let mut cont_a = String::new();
        fl_a.read_to_string(&mut cont_a).unwrap();
        println!("{}", cont_a);
    }
    {
        let mut fl_b = OpenOptions::new().read(true).write(true).open(fl_name).unwrap();
        fl_b.seek(SeekFrom::Start(2)).unwrap();
        fl_b.write(&[b'x', b'y']).unwrap();
        let mut byte_out = [0xFF ; 4];
        fl_b.read(&mut byte_out).unwrap();
        println!("{:?}", byte_out);
    }
    {
        let mut fl_c = OpenOptions::new().read(true).write(true).open(fl_name).unwrap();
        let mut cont = String::new();
        fl_c.read_to_string(&mut cont).unwrap();
        println!("{}", cont);
        
    }
}