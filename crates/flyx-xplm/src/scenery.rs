//! World coordinates and terrain probing.

use std::ffi::c_int;

use crate::sys;

/// Converts latitude/longitude (degrees) and elevation (metres MSL) to
/// X-Plane's local OpenGL coordinates.
pub fn world_to_local(latitude: f64, longitude: f64, elevation: f64) -> (f64, f64, f64) {
    let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
    unsafe { sys::XPLMWorldToLocal(latitude, longitude, elevation, &mut x, &mut y, &mut z) };
    (x, y, z)
}

/// Converts local OpenGL coordinates to latitude, longitude and elevation.
pub fn local_to_world(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let (mut lat, mut lon, mut alt) = (0.0, 0.0, 0.0);
    unsafe { sys::XPLMLocalToWorld(x, y, z, &mut lat, &mut lon, &mut alt) };
    (lat, lon, alt)
}

/// A reusable Y (vertical) terrain probe. Destroyed on drop. Main thread only.
pub struct TerrainProbe {
    probe: sys::XPLMProbeRef,
}

impl TerrainProbe {
    pub fn new() -> Self {
        let probe = unsafe { sys::XPLMCreateProbe(sys::xplm_ProbeY as sys::XPLMProbeType) };
        Self { probe }
    }

    /// Height (local Y) of the terrain directly below or above the local
    /// point, or `None` if the probe missed (e.g. scenery not loaded).
    pub fn ground_y(&self, x: f64, y: f64, z: f64) -> Option<f64> {
        let mut info = sys::XPLMProbeInfo_t {
            structSize: std::mem::size_of::<sys::XPLMProbeInfo_t>() as c_int,
            locationX: 0.0,
            locationY: 0.0,
            locationZ: 0.0,
            normalX: 0.0,
            normalY: 0.0,
            normalZ: 0.0,
            velocityX: 0.0,
            velocityY: 0.0,
            velocityZ: 0.0,
            is_wet: 0,
        };
        let result = unsafe {
            sys::XPLMProbeTerrainXYZ(self.probe, x as f32, y as f32, z as f32, &mut info)
        };
        (result as u32 == sys::xplm_ProbeHitTerrain).then_some(info.locationY as f64)
    }
}

impl Default for TerrainProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TerrainProbe {
    fn drop(&mut self) {
        unsafe { sys::XPLMDestroyProbe(self.probe) }
    }
}
