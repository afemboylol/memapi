#![allow(unused_qualifications)]

use alloc::{alloc::Layout};
use core::ptr::NonNull;

/// A trait containing constants for sized types.
pub trait SizedProps: Sized {
    /// The size of the type.
    const SZ: usize = size_of::<Self>();
    /// The alignment of the type.
    const ALIGN: usize = align_of::<Self>();
    /// The memory layout for the type.
    const LAYOUT: Layout = unsafe { Layout::from_size_align_unchecked(Self::SZ, Self::ALIGN) };

    /// Whether the type is zero-sized.
    const IS_ZST: bool = Self::SZ == 0;

    /// The largest safe length for a `[Self]`.
    const MAX_SLICE_LEN: usize = match Self::SZ {
        0 => usize::MAX,
        sz => (isize::MAX as usize) / sz,
    };
}

impl<T> SizedProps for T {}

/// A trait providing methods for pointers to provide the properties of their pointees.
pub trait PtrProps<T: ?Sized> {
    /// Gets the size of the value.
    ///
    /// # Safety
    ///
    /// The pointer must be valid.
    unsafe fn size(&self) -> usize;
    /// Gets the alignment of the value.
    ///
    /// # Safety
    ///
    /// The pointer must be valid.
    unsafe fn align(&self) -> usize;
    /// Gets the memory layout for the value.
    ///
    /// # Safety
    ///
    /// The pointer must be valid.
    unsafe fn layout(&self) -> Layout;

    #[cfg(feature = "metadata")]
    /// Gets the metadata of the value.
    ///
    /// # Safety
    ///
    /// The pointer must be valid.
    unsafe fn metadata(&self) -> <T as core::ptr::Pointee>::Metadata;

    /// Checks whether the value is zero-sized.
    ///
    /// # Safety
    ///
    /// The pointer must be valid.
    unsafe fn is_zst(&self) -> bool {
        self.size() == 0
    }

    /// Gets the largest safe length for a slice containing copies of `self`.
    ///
    /// # Safety
    ///
    /// The pointer must be valid.
    // this has almost no real use case as far as i can tell
    unsafe fn max_slice_len(&self) -> usize {
        match self.size() {
            0 => usize::MAX,
            sz => (isize::MAX as usize) / sz,
        }
    }
}

/// Implements [`PtrProps`] for a pointer type.
macro_rules! impl_ptr_props {
	($($name:ty $(,$to_ptr:ident)?)*) => {
		$(
		impl<T: ?Sized> PtrProps<T> for $name {
			unsafe fn size(&self) -> usize {
				// We use &*(*val) (?.to_ptr()?) to convert any primitive pointer type to a
                //  reference.
				// This is kind of a hack, but it lets us avoid *_of_val_raw, which is unstable.
				size_of_val::<T>(&*(*self)$(.$to_ptr())?)
			}

			unsafe fn align(&self) -> usize {
				align_of_val::<T>(&*(*self)$(.$to_ptr())?)
			}

			unsafe fn layout(&self) -> Layout {
				Layout::from_size_align_unchecked(
					self.size(),
					self.align()
				)
			}

			#[cfg(feature = "metadata")]
			unsafe fn metadata(&self) -> <T as core::ptr::Pointee>::Metadata {
				core::ptr::metadata(&*(*self)$(.$to_ptr())?)
			}
		}
		)*
	}
}

impl_ptr_props!(
    *const T
    *mut T

    &T
    &mut T

    NonNull<T>, as_ptr

    alloc::boxed::Box<T>
    alloc::rc::Rc<T>
    alloc::sync::Arc<T>
);
