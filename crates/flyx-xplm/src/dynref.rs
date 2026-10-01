//! Datarefs looked up by name at runtime, whose type is only known then
//! (aircraft profiles name them in a file).

use crate::dataref::{ArrayRef, DataRef};
use crate::sys;
use crate::util::to_cstring;

// `XPLMDataTypeID` bits from XPLMDataAccess.h (the bindings only carry the
// `xplm_` constants).
const TYPE_INT: i32 = 1;
const TYPE_FLOAT: i32 = 2;
const TYPE_DOUBLE: i32 = 4;
const TYPE_FLOAT_ARRAY: i32 = 8;
const TYPE_INT_ARRAY: i32 = 16;

/// A dataref value of whichever numeric type the dataref has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Int(i32),
    Float(f32),
    Double(f64),
}

impl Value {
    pub fn as_f64(self) -> f64 {
        match self {
            Value::Int(v) => v as f64,
            Value::Float(v) => v as f64,
            Value::Double(v) => v,
        }
    }
}

/// How a [`DynRef`] is accessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Int,
    Float,
    Double,
    IntArray,
    FloatArray,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DynRefError {
    #[error("dataref not found")]
    NotFound,
    #[error("dataref is an array; an index is needed")]
    NeedsIndex,
    #[error("dataref is not an array; it takes no index")]
    NotAnArray,
    #[error("index {index} is out of range (the array has {len} elements)")]
    IndexOutOfRange { index: usize, len: usize },
    #[error("unsupported dataref type (type bits {0})")]
    Unsupported(i32),
}

/// Picks how to access a dataref that reports the type bits `types`.
/// Datarefs may report several types; doubles and floats are preferred as
/// they hold every value the narrower types do.
pub fn select_kind(types: i32, indexed: bool) -> Result<Kind, DynRefError> {
    let has = |bit| types & bit != 0;
    let is_array = has(TYPE_FLOAT_ARRAY) || has(TYPE_INT_ARRAY);
    let is_scalar = has(TYPE_DOUBLE) || has(TYPE_FLOAT) || has(TYPE_INT);
    if indexed {
        if has(TYPE_FLOAT_ARRAY) {
            Ok(Kind::FloatArray)
        } else if has(TYPE_INT_ARRAY) {
            Ok(Kind::IntArray)
        } else if is_scalar {
            Err(DynRefError::NotAnArray)
        } else {
            Err(DynRefError::Unsupported(types))
        }
    } else if has(TYPE_DOUBLE) {
        Ok(Kind::Double)
    } else if has(TYPE_FLOAT) {
        Ok(Kind::Float)
    } else if has(TYPE_INT) {
        Ok(Kind::Int)
    } else if is_array {
        Err(DynRefError::NeedsIndex)
    } else {
        Err(DynRefError::Unsupported(types))
    }
}

/// A numeric dataref, or one element of an array dataref. Main thread only.
#[derive(Debug, Clone, Copy)]
pub struct DynRef {
    raw: sys::XPLMDataRef,
    kind: Kind,
    index: usize,
}

impl DynRef {
    /// Finds `name`, element `index` of it if it is an array.
    pub fn find(name: &str, index: Option<usize>) -> Result<Self, DynRefError> {
        let c_name = to_cstring(name);
        let raw = unsafe { sys::XPLMFindDataRef(c_name.as_ptr()) };
        if raw.is_null() {
            return Err(DynRefError::NotFound);
        }
        let types = unsafe { sys::XPLMGetDataRefTypes(raw) };
        let kind = select_kind(types, index.is_some())?;
        let index = index.unwrap_or(0);
        let this = Self { raw, kind, index };
        if matches!(kind, Kind::IntArray | Kind::FloatArray) {
            let len = this.array_len();
            // Some plugins report a length of 0 until they are initialised;
            // only reject indices past a known length.
            if len > 0 && index >= len {
                return Err(DynRefError::IndexOutOfRange { index, len });
            }
        }
        Ok(this)
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn is_writable(&self) -> bool {
        unsafe { sys::XPLMCanWriteDataRef(self.raw) != 0 }
    }

    pub fn get(&self) -> Value {
        match self.kind {
            Kind::Int => Value::Int(self.scalar::<i32>().get()),
            Kind::Float => Value::Float(self.scalar::<f32>().get()),
            Kind::Double => Value::Double(self.scalar::<f64>().get()),
            Kind::IntArray => Value::Int(self.array::<i32>().get_one(self.index)),
            Kind::FloatArray => Value::Float(self.array::<f32>().get_one(self.index)),
        }
    }

    /// Writes `value`, converted to the dataref's own type.
    pub fn set(&self, value: Value) {
        match self.kind {
            Kind::Int => self.scalar::<i32>().set(to_i32(value)),
            Kind::Float => self.scalar::<f32>().set(value.as_f64() as f32),
            Kind::Double => self.scalar::<f64>().set(value.as_f64()),
            Kind::IntArray => self.array::<i32>().set_one(self.index, to_i32(value)),
            Kind::FloatArray => self
                .array::<f32>()
                .set_one(self.index, value.as_f64() as f32),
        }
    }

    fn array_len(&self) -> usize {
        match self.kind {
            Kind::IntArray => self.array::<i32>().len(),
            Kind::FloatArray => self.array::<f32>().len(),
            _ => 0,
        }
    }

    fn scalar<T: crate::dataref::Scalar>(&self) -> DataRef<T> {
        DataRef::from_raw(self.raw)
    }

    fn array<T: crate::dataref::Element>(&self) -> ArrayRef<T> {
        ArrayRef::from_raw(self.raw)
    }
}

/// A dataref registered in X-Plane, from [`all_datarefs`].
#[derive(Debug, Clone)]
pub struct DataRefInfo {
    pub name: String,
    /// `XPLMDataTypeID` bits.
    pub types: i32,
    pub writable: bool,
}

impl DataRefInfo {
    pub fn is_array(&self) -> bool {
        self.types & (TYPE_FLOAT_ARRAY | TYPE_INT_ARRAY) != 0
    }

    pub fn is_numeric(&self) -> bool {
        self.types & (TYPE_INT | TYPE_FLOAT | TYPE_DOUBLE | TYPE_FLOAT_ARRAY | TYPE_INT_ARRAY) != 0
    }
}

/// Every dataref registered right now, by X-Plane and all plugins.
pub fn all_datarefs() -> Vec<DataRefInfo> {
    let count = unsafe { sys::XPLMCountDataRefs() }.max(0);
    let mut raws: Vec<sys::XPLMDataRef> = vec![std::ptr::null_mut(); count as usize];
    unsafe { sys::XPLMGetDataRefsByIndex(0, count, raws.as_mut_ptr()) };
    raws.into_iter()
        .filter(|raw| !raw.is_null())
        .filter_map(|raw| {
            let mut info = sys::XPLMDataRefInfo_t {
                structSize: std::mem::size_of::<sys::XPLMDataRefInfo_t>() as i32,
                name: std::ptr::null(),
                type_: 0,
                writable: 0,
                owner: 0,
            };
            unsafe { sys::XPLMGetDataRefInfo(raw, &mut info) };
            if info.name.is_null() {
                return None;
            }
            let name = unsafe { std::ffi::CStr::from_ptr(info.name) }
                .to_string_lossy()
                .into_owned();
            Some(DataRefInfo {
                name,
                types: info.type_,
                writable: info.writable != 0,
            })
        })
        .collect()
}

impl DynRef {
    /// Number of elements if this is an array dataref, else 0.
    pub fn len(&self) -> usize {
        self.array_len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn to_i32(value: Value) -> i32 {
    match value {
        Value::Int(v) => v,
        other => other.as_f64().round() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_prefer_the_widest_type() {
        assert_eq!(select_kind(TYPE_INT, false), Ok(Kind::Int));
        assert_eq!(select_kind(TYPE_FLOAT, false), Ok(Kind::Float));
        assert_eq!(
            select_kind(TYPE_FLOAT | TYPE_DOUBLE, false),
            Ok(Kind::Double)
        );
        assert_eq!(select_kind(TYPE_INT | TYPE_FLOAT, false), Ok(Kind::Float));
    }

    #[test]
    fn arrays_need_an_index_and_prefer_floats() {
        assert_eq!(select_kind(TYPE_INT_ARRAY, true), Ok(Kind::IntArray));
        assert_eq!(select_kind(TYPE_FLOAT_ARRAY, true), Ok(Kind::FloatArray));
        assert_eq!(
            select_kind(TYPE_INT_ARRAY | TYPE_FLOAT_ARRAY, true),
            Ok(Kind::FloatArray)
        );
        assert_eq!(
            select_kind(TYPE_FLOAT_ARRAY, false),
            Err(DynRefError::NeedsIndex)
        );
    }

    #[test]
    fn a_scalar_takes_no_index() {
        assert_eq!(select_kind(TYPE_INT, true), Err(DynRefError::NotAnArray));
    }

    #[test]
    fn byte_data_is_unsupported() {
        assert_eq!(select_kind(32, false), Err(DynRefError::Unsupported(32)));
        assert_eq!(select_kind(32, true), Err(DynRefError::Unsupported(32)));
    }

    #[test]
    fn values_convert_to_ints_by_rounding() {
        assert_eq!(to_i32(Value::Float(2.6)), 3);
        assert_eq!(to_i32(Value::Double(-1.4)), -1);
        assert_eq!(to_i32(Value::Int(7)), 7);
    }
}
