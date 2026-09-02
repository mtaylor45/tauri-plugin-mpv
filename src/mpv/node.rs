//! Conversion between `mpv_node` and `serde_json::Value`.
//!
//! This is the one part of the FFI layer that is pure logic, so it carries the unit tests: a
//! round-trip through the owned-node builder and back out through the reader exercises both
//! directions without needing a running mpv.

use std::ffi::{c_char, c_void, CStr, CString};

use serde_json::{Map, Value};

use super::ffi::*;
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// mpv_node -> JSON
// ---------------------------------------------------------------------------

/// Read an `mpv_node` into a `Value`.
///
/// # Safety
/// `node` must point at a valid `mpv_node` whose pointees are live for the duration of the call.
pub unsafe fn node_to_json(node: *const MpvNode) -> Value {
    if node.is_null() {
        return Value::Null;
    }
    let node = &*node;
    match node.format {
        MPV_FORMAT_NONE => Value::Null,
        MPV_FORMAT_STRING | MPV_FORMAT_OSD_STRING => {
            let s = node.u.string;
            if s.is_null() {
                Value::Null
            } else {
                Value::String(CStr::from_ptr(s).to_string_lossy().into_owned())
            }
        }
        MPV_FORMAT_FLAG => Value::Bool(node.u.flag != 0),
        MPV_FORMAT_INT64 => Value::Number(node.u.int64.into()),
        MPV_FORMAT_DOUBLE => serde_json::Number::from_f64(node.u.double_)
            .map(Value::Number)
            // NaN and the infinities have no JSON representation; null is the honest answer.
            .unwrap_or(Value::Null),
        MPV_FORMAT_NODE_ARRAY => {
            let list = node.u.list;
            if list.is_null() {
                return Value::Array(Vec::new());
            }
            let list = &*list;
            let n = list.num.max(0) as usize;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(node_to_json(list.values.add(i)));
            }
            Value::Array(out)
        }
        MPV_FORMAT_NODE_MAP => {
            let list = node.u.list;
            if list.is_null() {
                return Value::Object(Map::new());
            }
            let list = &*list;
            let n = list.num.max(0) as usize;
            let mut out = Map::with_capacity(n);
            for i in 0..n {
                if list.keys.is_null() {
                    break;
                }
                let key_ptr = *list.keys.add(i);
                let key = if key_ptr.is_null() {
                    continue;
                } else {
                    CStr::from_ptr(key_ptr).to_string_lossy().into_owned()
                };
                out.insert(key, node_to_json(list.values.add(i)));
            }
            Value::Object(out)
        }
        // Byte arrays only show up for a handful of binary properties this plugin does not
        // surface. Representing them as null keeps the shape predictable.
        MPV_FORMAT_BYTE_ARRAY => Value::Null,
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// JSON -> mpv_node
// ---------------------------------------------------------------------------

/// Backing allocations for a built `mpv_node` tree.
///
/// Every element is individually boxed (or, for `CString`, owns its own heap buffer), so growing
/// these vectors never moves the memory the node tree points into.
// clippy::vec_box is a false positive here and the boxing is load-bearing: the node tree holds
// raw pointers into these elements, so they must not move when the vector grows. `round_trips_many_siblings`
// exercises exactly that.
#[allow(clippy::vec_box)]
#[derive(Default)]
struct Arena {
    strings: Vec<CString>,
    node_arrays: Vec<Box<[MpvNode]>>,
    key_arrays: Vec<Box<[*mut c_char]>>,
    lists: Vec<Box<MpvNodeList>>,
}

/// An `mpv_node` tree owned on the Rust side, valid for as long as this value lives.
///
/// Hand `as_ptr()` to libmpv for the duration of a call, then drop it. Never pass it to
/// `mpv_free_node_contents` — libmpv did not allocate any of this.
pub struct OwnedNode {
    node: MpvNode,
    _arena: Arena,
}

impl OwnedNode {
    pub fn as_ptr(&self) -> *const MpvNode {
        &self.node
    }

    pub fn as_mut_ptr(&mut self) -> *mut MpvNode {
        &mut self.node
    }
}

/// Build an owned `mpv_node` tree from a JSON value.
pub fn json_to_node(value: &Value) -> Result<OwnedNode> {
    let mut arena = Arena::default();
    let node = build(value, &mut arena)?;
    Ok(OwnedNode {
        node,
        _arena: arena,
    })
}

fn cstring(s: &str, arena: &mut Arena) -> Result<*mut c_char> {
    let c = CString::new(s)
        .map_err(|_| Error::InvalidArgument(format!("string contains a NUL byte: {s:?}")))?;
    let ptr = c.as_ptr() as *mut c_char;
    arena.strings.push(c);
    Ok(ptr)
}

fn build(value: &Value, arena: &mut Arena) -> Result<MpvNode> {
    Ok(match value {
        Value::Null => MpvNode::none(),
        Value::Bool(b) => MpvNode {
            u: MpvNodeUnion {
                flag: i32::from(*b),
            },
            format: MPV_FORMAT_FLAG,
        },
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                MpvNode {
                    u: MpvNodeUnion { int64: i },
                    format: MPV_FORMAT_INT64,
                }
            } else {
                let f = n.as_f64().ok_or_else(|| {
                    Error::InvalidArgument(format!("number is not representable: {n}"))
                })?;
                MpvNode {
                    u: MpvNodeUnion { double_: f },
                    format: MPV_FORMAT_DOUBLE,
                }
            }
        }
        Value::String(s) => MpvNode {
            u: MpvNodeUnion {
                string: cstring(s, arena)?,
            },
            format: MPV_FORMAT_STRING,
        },
        Value::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(build(item, arena)?);
            }
            let mut boxed: Box<[MpvNode]> = values.into_boxed_slice();
            let values_ptr = boxed.as_mut_ptr();
            arena.node_arrays.push(boxed);

            let mut list = Box::new(MpvNodeList {
                num: items.len() as i32,
                values: values_ptr,
                keys: std::ptr::null_mut(),
            });
            let list_ptr = list.as_mut() as *mut MpvNodeList;
            arena.lists.push(list);

            MpvNode {
                u: MpvNodeUnion { list: list_ptr },
                format: MPV_FORMAT_NODE_ARRAY,
            }
        }
        Value::Object(map) => {
            let mut values = Vec::with_capacity(map.len());
            let mut keys = Vec::with_capacity(map.len());
            for (k, v) in map {
                values.push(build(v, arena)?);
                keys.push(cstring(k, arena)?);
            }
            let mut boxed_values: Box<[MpvNode]> = values.into_boxed_slice();
            let values_ptr = boxed_values.as_mut_ptr();
            arena.node_arrays.push(boxed_values);

            let mut boxed_keys: Box<[*mut c_char]> = keys.into_boxed_slice();
            let keys_ptr = boxed_keys.as_mut_ptr();
            arena.key_arrays.push(boxed_keys);

            let mut list = Box::new(MpvNodeList {
                num: map.len() as i32,
                values: values_ptr,
                keys: keys_ptr,
            });
            let list_ptr = list.as_mut() as *mut MpvNodeList;
            arena.lists.push(list);

            MpvNode {
                u: MpvNodeUnion { list: list_ptr },
                format: MPV_FORMAT_NODE_MAP,
            }
        }
    })
}

/// A node whose contents libmpv allocated, freed on drop via `mpv_free_node_contents`.
pub struct MpvOwnedNode {
    pub node: MpvNode,
}

impl MpvOwnedNode {
    pub fn empty() -> Self {
        MpvOwnedNode {
            node: MpvNode::none(),
        }
    }

    pub fn to_json(&self) -> Value {
        unsafe { node_to_json(&self.node) }
    }
}

impl Drop for MpvOwnedNode {
    fn drop(&mut self) {
        if let Ok(lib) = super::loader::get() {
            unsafe { (lib.free_node_contents)(&mut self.node) };
        }
    }
}

/// A string libmpv allocated, freed on drop via `mpv_free`.
pub struct MpvString(pub *mut c_char);

impl MpvString {
    pub fn to_rust(&self) -> Option<String> {
        if self.0.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(self.0) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }
}

impl Drop for MpvString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            if let Ok(lib) = super::loader::get() {
                unsafe { (lib.free)(self.0 as *mut c_void) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a node tree from JSON and read it straight back. This exercises the arena's pointer
    /// stability as well as both conversion directions.
    fn round_trip(value: Value) -> Value {
        let owned = json_to_node(&value).expect("build");
        unsafe { node_to_json(owned.as_ptr()) }
    }

    #[test]
    fn round_trips_scalars() {
        assert_eq!(round_trip(json!(null)), json!(null));
        assert_eq!(round_trip(json!(true)), json!(true));
        assert_eq!(round_trip(json!(false)), json!(false));
        assert_eq!(round_trip(json!(42)), json!(42));
        assert_eq!(round_trip(json!(-7)), json!(-7));
        assert_eq!(round_trip(json!("loadfile")), json!("loadfile"));
        assert_eq!(round_trip(json!("")), json!(""));
    }

    #[test]
    fn round_trips_doubles() {
        let out = round_trip(json!(1.5));
        assert_eq!(out.as_f64().unwrap(), 1.5);
    }

    #[test]
    fn round_trips_a_typical_command() {
        let cmd = json!(["loadfile", "/tmp/a b.mkv", "replace"]);
        assert_eq!(round_trip(cmd.clone()), cmd);
    }

    #[test]
    fn round_trips_nested_structures() {
        // Deliberately wide and deep: many arena pushes, so any pointer invalidated by a Vec
        // reallocation would show up as corruption here.
        let value = json!({
            "a": [1, 2, 3, {"deep": ["x", "y", {"deeper": true}]}],
            "b": {"c": {"d": {"e": "leaf"}}},
            "empty_array": [],
            "empty_map": {},
            "n": null,
        });
        assert_eq!(round_trip(value.clone()), value);
    }

    #[test]
    fn round_trips_many_siblings() {
        let items: Vec<Value> = (0..256)
            .map(|i| json!({ "i": i, "s": format!("v{i}") }))
            .collect();
        let value = Value::Array(items);
        assert_eq!(round_trip(value.clone()), value);
    }

    #[test]
    fn rejects_interior_nul() {
        let result = json_to_node(&json!("bad\0string"));
        assert!(matches!(result, Err(Error::InvalidArgument(_))));
    }

    #[test]
    fn non_finite_doubles_become_null() {
        let node = MpvNode {
            u: MpvNodeUnion { double_: f64::NAN },
            format: MPV_FORMAT_DOUBLE,
        };
        assert_eq!(unsafe { node_to_json(&node) }, Value::Null);
    }

    #[test]
    fn null_node_pointer_is_null_json() {
        assert_eq!(unsafe { node_to_json(std::ptr::null()) }, Value::Null);
    }
}
