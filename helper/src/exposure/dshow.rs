//! A Windows webcam's exposure, through DirectShow's IAMCameraControl. A value set there holds while
//! SDL streams the same camera through Media Foundation (measured on a Framework webcam).

use windows::core::{w, GUID};
use windows::Win32::Media::DirectShow::{
    CameraControl_Exposure, CameraControl_Flags_Manual, IAMCameraControl, ICreateDevEnum,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IBindCtx, IEnumMoniker, IErrorLog, IMoniker,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_BSTR};

const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);
const CLSID_VIDEO_INPUT_DEVICE_CATEGORY: GUID =
    GUID::from_u128(0x860bb310_5d01_11d0_bd3b_00a0c911ce86);
const EXPOSURE: i32 = CameraControl_Exposure.0;
const MANUAL: i32 = CameraControl_Flags_Manual.0;

/// In log2 seconds, all DirectShow offers: 7.8, 15.6 and 31 ms. A Framework webcam read the Shell's
/// code at 15.6 ms; 3.9 ms left it too dark to read.
pub const LADDER: &[i32] = &[-7, -6, -5];
pub const START: usize = 1;
/// Below this, a code or the middle of the picture is too dim: manual exposure here runs darker
/// than on Linux, and the decoders read a code at 120.
pub const DIM: u8 = 100;

pub struct Device {
    cc: IAMCameraControl,
    key: String,
    range: (i32, i32),
}

/// A device's name as DirectShow lists it, which is the name SDL gives the camera.
unsafe fn friendly_name(m: &IMoniker) -> Option<String> {
    let bag: IPropertyBag = m.BindToStorage(None::<&IBindCtx>, None::<&IMoniker>).ok()?;
    let mut v = VARIANT::default();
    bag.Read(w!("FriendlyName"), &mut v, None::<&IErrorLog>)
        .ok()?;
    let name = (v.Anonymous.Anonymous.vt == VT_BSTR)
        .then(|| v.Anonymous.Anonymous.Anonymous.bstrVal.to_string());
    let _ = VariantClear(&mut v);
    name
}

impl Device {
    /// The camera SDL named `camera` (else the only one there is), when it lets its exposure be set.
    pub fn find(camera: &str) -> Option<Device> {
        // SAFETY: COM calls on this thread, which joins the multithreaded apartment first; every
        // out-pointer is a local the call fills.
        unsafe {
            // Already joined, or SDL made this thread single-threaded: either way COM works here.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let devices: ICreateDevEnum =
                CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER).ok()?;
            let mut list: Option<IEnumMoniker> = None;
            devices
                .CreateClassEnumerator(&CLSID_VIDEO_INPUT_DEVICE_CATEGORY, &mut list, 0)
                .ok()?;
            let list = list?;
            let mut found = Vec::new();
            loop {
                let mut one = [None];
                if list.Next(&mut one, None).is_err() {
                    break;
                }
                let Some(m) = one[0].take() else { break };
                let name = friendly_name(&m).unwrap_or_default();
                if let Ok(cc) =
                    m.BindToObject::<_, _, IAMCameraControl>(None::<&IBindCtx>, None::<&IMoniker>)
                {
                    found.push((name, cc));
                }
            }
            let at = found
                .iter()
                .position(|(n, _)| n.trim() == camera.trim())
                .or((found.len() == 1).then_some(0))?;
            let (name, cc) = found.swap_remove(at);
            let (mut min, mut max, mut step, mut default, mut caps) = (0, 0, 0, 0, 0);
            cc.GetRange(
                EXPOSURE,
                &mut min,
                &mut max,
                &mut step,
                &mut default,
                &mut caps,
            )
            .ok()?;
            if caps & MANUAL == 0 || max <= min {
                return None;
            }
            let key = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            Some(Device {
                cc,
                key,
                range: (min, max),
            })
        }
    }

    /// What names this camera in the state file's name.
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn range(&self) -> (i32, i32) {
        self.range
    }

    /// The camera's own flags (auto or manual) and value.
    pub fn own(&self) -> Option<(i32, i32)> {
        let (mut value, mut flags) = (0, 0);
        // SAFETY: both out-pointers are locals the call fills.
        unsafe { self.cc.Get(EXPOSURE, &mut value, &mut flags) }.ok()?;
        Some((flags, value))
    }

    /// Manual at the value it has: `set` picks the value.
    pub fn manual(&self) -> bool {
        self.own().is_some_and(|(_, v)| self.set(v))
    }

    pub fn set(&self, value: i32) -> bool {
        // SAFETY: a plain COM call on the interface this value holds.
        unsafe { self.cc.Set(EXPOSURE, value, MANUAL) }.is_ok()
    }

    pub fn give_back(&self, (flags, value): (i32, i32)) {
        // SAFETY: as in `set`.
        let _ = unsafe { self.cc.Set(EXPOSURE, value, flags) };
    }
}
