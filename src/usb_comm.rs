use crate::*;
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
    had_kernel : bool,
}

impl<'a> UsbClient<'a> {
    pub fn from_device(device: &mut Device<'a>) -> Result<UsbClient<'a>, RawStringErr> {
        let desc = device.device_descriptor().unwrap();
        let (rd, wd) = UsbClient::find_bulk_endpoints(device, &desc).unwrap();
        let mut hndl = device.open().map_err(|e| format!("Open err: {:?}", e)).unwrap();
        let had_kernel = if hndl.kernel_driver_active(rd.0.iface).map_err(|e| format!("Error checking kernel: {:?}", e)).unwrap() {
            hndl.detach_kernel_driver(rd.0.iface).map_err(|e| format!("Found kernel detach err: {:?}", e)).unwrap();
            true
        } else {false};
        hndl.reset().map_err(|e| format!("Found reset err: {:?}", e)).unwrap();
        hndl.set_active_configuration(rd.0.config).map_err(|e| format!("Could not set active config: {:?}", e)).unwrap();
        hndl.claim_interface(rd.0.iface).map_err(|e| format!("Could not claim iface {}: {:?}", rd.0.iface, e)).unwrap();
        Ok(UsbClient::new(hndl, rd, wd, had_kernel))
    }
    pub fn new(
        device_handle: DeviceHandle<'a>,
        read_endpoint: ReadEndpoint,
        write_endpoint: WriteEndpoint,
        had_kernel : bool,
    ) -> UsbClient<'a> {
        UsbClient {
            device_handle,
            read_endpoint,
            write_endpoint,
            had_kernel,
        }
    }

    pub fn pull_bytes(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let endpoint = self.read_endpoint.0;
        let timeout = Duration::from_secs(30);
        let rval = self
            .device_handle
            .read_bulk(endpoint.address, buffer, timeout)
            .map_err(|e| format!("Read Error: {:?}", e)).unwrap();
        Ok(rval)
    }

    pub fn push_bytes(&mut self, buffer: &[u8]) -> Result<usize, String> {
        let endpoint = self.write_endpoint.0;
        let timeout = Duration::from_secs(30);
        let rval = self
            .device_handle
            .write_bulk(endpoint.address, buffer, timeout)
            .map_err(|e| format!("Write Error: {:?}", e)).unwrap();
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
    }
}

impl <'a> scsi::CommunicationChannel for UsbClient<'a> {
    fn in_transfer<B : scsi::Buffer> (&mut self, buffer: &mut B) -> Result<usize, scsi::ScsiError> {
        let mut shim = Vec::with_capacity(buffer.capacity());
        shim.resize(buffer.capacity(), 0);
        let rval = self.pull_bytes(shim.as_mut_slice())
        .map_err(|e|{ 
            eprintln!("Got error in read: {:?}", e);
            scsi::ScsiError::from_cause(scsi::ErrorCause::UsbTransferError{ direction: scsi::UsbTransferDirection::In})
        }).unwrap();
        for byte in shim {
            buffer.push_byte(byte).unwrap();
        }
        Ok(rval)
    }

    fn out_transfer<B : scsi::Buffer>(&mut self, bytes: &mut B) -> Result<usize, scsi::ScsiError> {
        let mut shim = Vec::with_capacity(bytes.size());
        while bytes.size() > 0 {
            shim.push(bytes.pull_byte()?);
        }
        let rval = self.push_bytes(shim.as_ref())
        .map_err(|e|{ 
            eprintln!("Got error in write: {:?}", e);
            scsi::ScsiError::from_cause(scsi::ErrorCause::UsbTransferError{ direction: scsi::UsbTransferDirection::Out})
        }).unwrap();
        Ok(rval)
    }
}