
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawStringErr {
    pub err : String, 
}
impl From<scsi::ScsiError> for RawStringErr {
    fn from(obj : scsi::ScsiError) -> RawStringErr {
        RawStringErr {
            err : format!("scsi::ScsiError ({:?})", obj)
        }
    }
}

impl From<mbr_nostd::MbrError> for RawStringErr {
    fn from(obj : mbr_nostd::MbrError) -> RawStringErr {
        RawStringErr {
            err : format!("mbr_nostd::MbrError ({:?})", obj)
        }
    }
}
impl From<libusb::Error> for RawStringErr {
    fn from(obj : libusb::Error) -> RawStringErr {
        RawStringErr {
            err : format!("libusb::Error ({:?})", obj)
        }
    }
}

impl From<String> for RawStringErr {
    fn from(err : String) -> RawStringErr {
        RawStringErr{
            err
        }
    }
}

impl <'a> From<&'a str> for RawStringErr {
    fn from(err : &str) -> RawStringErr {
        RawStringErr {
            err : err.to_owned()
        }
    }
}

impl From<std::io::Error> for RawStringErr {
    fn from(err : std::io::Error) -> RawStringErr {
        RawStringErr {
            err : format!("std::io::Error ({:?})", err)
        }
    }
}