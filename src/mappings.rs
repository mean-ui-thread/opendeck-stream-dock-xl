use mirajazz::{
    device::DeviceQuery,
    types::{HidDeviceInfo, ImageFormat, ImageMirroring, ImageMode, ImageRotation},
};

// Must be unique between all the plugins, 2 characters long and match `DeviceNamespace` field in `manifest.json`
pub const DEVICE_NAMESPACE: &str = "xl";

pub const ROW_COUNT: usize = 4;
pub const COL_COUNT: usize = 8;
pub const KEY_COUNT: usize = ROW_COUNT * COL_COUNT;
pub const ENCODER_COUNT: usize = 2;
pub const DEVICE_TYPE: u8 = 4; // 4 = "Second revision of Stream Deck XL"

#[derive(Debug, Clone)]
pub enum Kind {
    MiraBoxStreamDockXL,
}

pub const MIRABOX_VID: u16 = 0x5548;
pub const STREAM_DOCK_XL_PID: u16 = 0x1031;

// Map all queries to usage page 65440 and usage id 1 for now
pub const MIRABOX_STREAM_DOCK_XL_QUERY: DeviceQuery = DeviceQuery::new(65440, 1, MIRABOX_VID, STREAM_DOCK_XL_PID);

pub const QUERIES: [DeviceQuery; 1] = [
    MIRABOX_STREAM_DOCK_XL_QUERY,
];

pub fn image_format() -> ImageFormat {
    ImageFormat {
        mode: ImageMode::JPEG,
        size: (80, 80), // from https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/bc08f2cffceb03b01adda185d056c8e8c824a480/CPP-SDK/src/HotspotDevice/StreamDockXL/streamdockXL.cpp#L49-L50
        rotation: ImageRotation::Rot180, // from https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/bc08f2cffceb03b01adda185d056c8e8c824a480/CPP-SDK/src/HotspotDevice/StreamDockXL/streamdockXL.cpp#L53
        mirror: ImageMirroring::None,
    }
}

impl Kind {
    /// Matches devices VID+PID pairs to correct kinds
    pub fn from_vid_pid(vid: u16, pid: u16) -> Option<Self> {
        match vid {

            MIRABOX_VID => match pid {
                STREAM_DOCK_XL_PID => Some(Kind::MiraBoxStreamDockXL),
                _ => None,
            },

            _ => None,
        }
    }

    /// Returns protocol version for device
    pub fn protocol_version(&self) -> usize {
        #[allow(clippy::match_single_binding)]
        match self {
            _ => 3,
        }
    }

    /// There is no point relying on manufacturer/device names reported by the USB stack,
    /// so we return custom names for all the kinds of devices
    pub fn human_name(&self) -> String {
        match &self {
            Self::MiraBoxStreamDockXL => "Mirabox Stream Dock XL",
        }
        .to_string()
    }

    /// Whether the device is capable of reporting both EncoderUp and EncoderDown states
    pub fn supports_both_encoder_states(&self) -> bool {
        match &self {
            Self::MiraBoxStreamDockXL => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CandidateDevice {
    pub id: String,
    pub dev: HidDeviceInfo,
    pub kind: Kind,
}
