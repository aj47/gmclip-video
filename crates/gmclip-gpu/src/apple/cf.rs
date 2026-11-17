//! Small CoreFoundation helpers shared by the Apple modules.

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type,
};

use crate::GpuError;

/// Maps a non-zero `OSStatus` / `CVReturn` to [`GpuError::Os`].
pub(crate) fn check(status: i32, step: &'static str) -> Result<(), GpuError> {
    if status == 0 {
        Ok(())
    } else {
        Err(GpuError::Os {
            step,
            status: status as i64,
        })
    }
}

/// A CF value for a heterogeneous attribute dictionary.
#[allow(dead_code)] // the session property builders use the rest (H-02, H-08)
pub(crate) enum Val<'a> {
    I32(i32),
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(&'a CFString),
    Obj(&'a CFType),
    EmptyDict,
}

/// Builds an immutable `CFDictionary<CFString, CFType>` from `(key, value)` pairs.
pub(crate) fn dict(pairs: &[(&CFString, Val<'_>)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let mut owned: Vec<CFRetained<CFType>> = Vec::with_capacity(pairs.len());
    for (_, v) in pairs {
        let o: CFRetained<CFType> = match v {
            Val::I32(n) => up(&CFNumber::new_i32(*n)),
            Val::I64(n) => up(&CFNumber::new_i64(*n)),
            Val::F64(n) => up(&CFNumber::new_f64(*n)),
            Val::Bool(b) => up(CFBoolean::new(*b)),
            Val::Str(s) => up(*s),
            Val::Obj(o) => o.retain(),
            Val::EmptyDict => up(&CFDictionary::<CFString, CFType>::empty()),
        };
        owned.push(o);
    }
    let keys: Vec<&CFString> = pairs.iter().map(|(k, _)| *k).collect();
    let vals: Vec<&CFType> = owned.iter().map(|o| &**o).collect();
    CFDictionary::from_slices(&keys, &vals)
}

/// Retains any CF object as a plain `CFType`.
pub(crate) fn up<T: AsRef<CFType> + ?Sized>(v: &T) -> CFRetained<CFType> {
    v.as_ref().retain()
}
