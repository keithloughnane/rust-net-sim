use slotmap::{Key, KeyData, new_key_type};

new_key_type! {
    /// Handle to a [`ControlNode`](crate::ControlNode) in a [`Network`](crate::Network).
    ///
    /// Cheap to copy. A handle to a removed node never aliases a newer node.
    pub struct NodeId;

    /// Handle to a [`Link`](crate::Link) in a [`Network`](crate::Network).
    ///
    /// Cheap to copy. A handle to a removed link never aliases a newer link.
    pub struct LinkId;
}

macro_rules! impl_raw {
    ($ty:ty) => {
        impl $ty {
            /// Converts the handle to a plain integer, for passing across an FFI boundary.
            ///
            /// Never returns 0, so hosts can use 0 to mean "no handle".
            #[must_use]
            pub fn to_raw(self) -> u64 {
                self.data().as_ffi()
            }

            /// Rebuilds a handle from [`to_raw`](Self::to_raw).
            ///
            /// Any value is safe to pass: one that did not come from `to_raw` produces a handle
            /// that simply does not resolve to anything.
            #[must_use]
            pub fn from_raw(raw: u64) -> Self {
                KeyData::from_ffi(raw).into()
            }
        }
    };
}

impl_raw!(NodeId);
impl_raw!(LinkId);
