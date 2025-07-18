use crate::helpers::{alloc_slice, dealloc_n};
use crate::{
    error::AllocError,
    grow,
    helpers::{layout_or_sz_align, AllocGuard, SliceAllocGuard},
    ralloc, shrink,
    type_props::{PtrProps, SizedProps},
    Alloc, AllocPattern,
};
use core::{alloc::Layout, mem::MaybeUninit, ptr::NonNull};
// TODO: slice growth with filling, patterning, initializing, etc., slice shrinking with init elem 
//  dropping

macro_rules! realloc {
    ($fun:ident, $self:ident, $ptr:ident, $len:expr, $new_len:expr, $ty:ty $(,$pat:ident)?) => {
		$fun(
            $self,
            $ptr.cast(),
            Layout::from_size_align_unchecked($len * <$ty>::SZ, <$ty>::ALIGN),
            layout_or_sz_align::<$ty>($new_len)
                .map_err(|(sz, aln)| AllocError::LayoutError(sz, aln))?,
            $(AllocPattern::<fn(usize) -> u8>::$pat)?
        )
        .map(NonNull::cast)
	};
}

/// Slice-specific extension methods for [`Alloc`], providing convenient functions for slice
/// allocator operations.
pub trait AllocSlice: Alloc {
    /// Attempts to allocate a block of memory for `len` instances of `T`.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    fn alloc_slice<T>(&self, len: usize) -> Result<NonNull<[T]>, AllocError> {
        alloc_slice(self, len, Self::alloc)
    }

    /// Attempts to allocate a zeroed block of memory for `len` instances of `T`.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    fn alloc_slice_zeroed<T>(&self, len: usize) -> Result<NonNull<[T]>, AllocError> {
        alloc_slice(self, len, Self::alloc_zeroed)
    }

    /// Allocates uninitialized memory for a slice of `T` and clones each element.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::ZeroSizedLayout`] if the slice is zero-sized.
    #[track_caller]
    #[inline]
    fn alloc_clone_slice_to<T: Clone>(&self, data: &[T]) -> Result<NonNull<[T]>, AllocError> {
        match self.alloc(unsafe { data.layout() }) {
            Ok(ptr) => Ok(unsafe {
                let mut guard = SliceAllocGuard::new(ptr.cast(), self, data.len());
                for elem in data {
                    guard.init_unchecked(elem.clone());
                }
                guard.release()
            }),
            Err(e) => Err(e),
        }
    }

    /// Allocates uninitialized memory for a `[T]` of length `len` and fills each element
    /// with the result of `f(elem_idx)`.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[track_caller]
    #[inline]
    fn alloc_slice_with<T, F: Fn(usize) -> T>(
        &self,
        len: usize,
        f: F,
    ) -> Result<NonNull<[T]>, AllocError> {
        match self.alloc(
            layout_or_sz_align::<T>(len).map_err(|(sz, aln)| AllocError::LayoutError(sz, aln))?,
        ) {
            Ok(ptr) => Ok(unsafe {
                let mut guard = SliceAllocGuard::new(ptr.cast(), self, len);
                for i in 0..len {
                    guard.init_unchecked(f(i));
                }
                guard.release()
            }),
            Err(e) => Err(e),
        }
    }

    /// Allocates uninitialized memory for a `[T]` of length `len` and initializes it using `init`.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    #[track_caller]
    #[inline]
    fn alloc_init_slice<T, I: Fn(NonNull<[T]>)>(
        &self,
        init: I,
        len: usize,
    ) -> Result<NonNull<[T]>, AllocError> {
        let guard = AllocGuard::new(self.alloc_slice(len)?, self);
        init(*guard);
        Ok(guard.release())
    }

    /// Allocates uninitialized memory for a `[T]` of length `len` and fills each element
    /// with `T`'s default value.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[track_caller]
    #[inline]
    fn alloc_default_slice<T: Default>(&self, len: usize) -> Result<NonNull<[T]>, AllocError> {
        self.alloc_slice_with(len, |_| T::default())
    }

    /// Drops `init` elements from a partially initialized slice and deallocates it.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a block of memory allocated using this allocator, be valid for reads
    ///   and writes, aligned, a valid `[T]` for `init` elements, and a valid `[MaybeUninit<T>]`.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn drop_and_dealloc_uninit_slice<T>(&self, ptr: NonNull<[MaybeUninit<T>]>, init: usize) {
        NonNull::slice_from_raw_parts(ptr.cast::<T>(), init).drop_in_place();
        self.dealloc(ptr.cast::<u8>(), ptr.layout());
    }

    /// Drops `init` elements from a partially initialized slice, then zeroes and deallocates its
    /// memory.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a block of memory allocated using this allocator, be valid for reads
    ///   and writes, aligned, and a valid `T`.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn drop_zero_and_dealloc_uninit_slice<T>(
        &self,
        ptr: NonNull<[MaybeUninit<T>]>,
        init: usize,
    ) {
        NonNull::slice_from_raw_parts(ptr.cast::<T>(), init).drop_in_place();
        ptr.cast::<T>().write_bytes(0, ptr.len());
        self.dealloc(ptr.cast::<u8>(), ptr.layout());
    }

    /// Allocates memory for a slice of uninitialized `T` with the given length and returns a
    /// [`SliceAllocGuard`] around it to ensure proper destruction and deallocation on panic.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    fn alloc_slice_guard<T>(
        &'_ self,
        len: usize,
    ) -> Result<SliceAllocGuard<'_, T, Self>, AllocError> {
        match self.alloc_slice::<T>(len) {
            Ok(ptr) => Ok(SliceAllocGuard::new(ptr.cast(), self, len)),
            Err(e) => Err(e),
        }
    }

    /// Grows a slice to a new length.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::GrowSmallerNewLayout`] if `new_len < slice.len()`.
    ///
    /// # Safety
    ///
    /// - `slice` must point to a slice allocated using this allocator.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn grow_slice<T>(
        &self,
        slice: NonNull<[T]>,
        new_len: usize,
    ) -> Result<NonNull<[T]>, AllocError> {
        self.grow_raw_slice(slice.cast::<T>(), slice.len(), new_len)
            .map(|p| NonNull::slice_from_raw_parts(p, new_len))
    }

    /// Grows a slice to a new length given the pointer to its first element, current length, and
    /// requested length.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::GrowSmallerNewLayout`] if `new_len < slice.len()`.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a slice allocated using this allocator.
    /// - `len` must describe exactly the number of initialized elements in that slice.
    // Safety #2 implies that `len` must be a valid length for the slice (which is required because
    // we use from_size_align_unchecked)
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn grow_raw_slice<T>(
        &self,
        ptr: NonNull<T>,
        len: usize,
        new_len: usize,
    ) -> Result<NonNull<T>, AllocError> {
        realloc!(grow, self, ptr, len, new_len, T, None)
    }

    /// Grows a slice to a new length, zeroing any newly allocated bytes.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::GrowSmallerNewLayout`] if `new_len < slice.len()`.
    ///
    /// # Safety
    ///
    /// - `slice` must point to a slice allocated using this allocator.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn grow_slice_zeroed<T>(
        &self,
        slice: NonNull<[T]>,
        new_len: usize,
    ) -> Result<NonNull<[T]>, AllocError> {
        self.grow_raw_slice_zeroed(slice.cast::<T>(), slice.len(), new_len)
            .map(|p| NonNull::slice_from_raw_parts(p, new_len))
    }

    /// Grows a slice to a new length given the pointer to its first element, current length, and
    /// requested length, zeroing any newly allocated bytes.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::GrowSmallerNewLayout`] if `new_len < slice.len()`.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a slice allocated using this allocator.
    /// - `len` must describe exactly the number of initialized elements in that slice.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn grow_raw_slice_zeroed<T>(
        &self,
        ptr: NonNull<T>,
        len: usize,
        new_len: usize,
    ) -> Result<NonNull<T>, AllocError> {
        realloc!(grow, self, ptr, len, new_len, T, Zero)
    }

    /// Shrinks a slice to a new length without dropping any removed elements.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::ShrinkBiggerNewLayout`] if `new_len > slice.len()`.
    ///
    /// # Safety
    ///
    /// - `slice` must point to a slice allocated using this allocator.
    unsafe fn shrink_slice<T>(
        &self,
        slice: NonNull<[T]>,
        new_len: usize,
    ) -> Result<NonNull<[T]>, AllocError> {
        self.shrink_raw_slice(slice.cast::<T>(), slice.len(), new_len)
            .map(|p| NonNull::slice_from_raw_parts(p, new_len))
    }

    /// Shrinks a slice to a new length given the pointer to its first element, current length, and
    /// requested length. This does not drop any removed elements.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::ShrinkBiggerNewLayout`] if `new_len > slice.len()`.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a slice allocated using this allocator.
    /// - `len` must describe exactly the number of initialized elements in that slice.
    unsafe fn shrink_raw_slice<T>(
        &self,
        ptr: NonNull<T>,
        len: usize,
        new_len: usize,
    ) -> Result<NonNull<T>, AllocError> {
        realloc!(shrink, self, ptr, len, new_len, T)
    }

    /// Reallocates a slice to a new length without dropping any removed elements.
    ///
    /// On grow, preserves all existing elements and truncates to `new_len` elements on shrink.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    ///
    /// # Safety
    ///
    /// - `slice` must point to a slice allocated using this allocator.
    unsafe fn realloc_slice<T>(
        &self,
        slice: NonNull<[T]>,
        new_len: usize,
    ) -> Result<NonNull<T>, AllocError> {
        self.realloc_raw_slice(slice.cast::<T>(), slice.len(), new_len)
    }

    /// Reallocates a slice to a new length given the pointer to its first element, current length,
    /// and requested length.
    ///
    /// On grow, preserves all existing elements and truncates to `new_len` elements on shrink. This
    /// does not drop any removed elements.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
    /// - [`AllocError::ShrinkBiggerNewLayout`] if `new_len > slice.len()`.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a slice allocated using this allocator.
    /// - `len` must describe exactly the number of initialized elements in that slice.
    unsafe fn realloc_raw_slice<T>(
        &self,
        ptr: NonNull<T>,
        len: usize,
        new_len: usize,
    ) -> Result<NonNull<T>, AllocError> {
        realloc!(ralloc, self, ptr, len, new_len, T, None)
    }

	/// Reallocates a slice to a new length without dropping any removed elements.
	///
	/// On grow, preserves all existing elements and truncates to `new_len` elements on shrink.
	/// 
	/// Any newly allocated bytes will be zeroed.
	///
	/// # Errors
	///
	/// - [`AllocError::AllocFailed`] if allocation fails.
	/// - [`AllocError::LayoutError`] if the computed layout is invalid.
	/// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
	///
	/// # Safety
	///
	/// - `slice` must point to a slice allocated using this allocator.
	unsafe fn realloc_slice_zeroed<T>(
		&self,
		slice: NonNull<[T]>,
		new_len: usize,
	) -> Result<NonNull<T>, AllocError> {
		self.realloc_raw_slice_zeroed(slice.cast::<T>(), slice.len(), new_len)
	}

	/// Reallocates a slice to a new length given the pointer to its first element, current length,
	/// and requested length.
	///
	/// On grow, preserves all existing elements and truncates to `new_len` elements on shrink. This
	/// does not drop any removed elements.
	///
	/// Any newly allocated bytes will be zeroed.
	///
	/// # Errors
	///
	/// - [`AllocError::AllocFailed`] if allocation fails.
	/// - [`AllocError::LayoutError`] if the computed layout is invalid.
	/// - [`AllocError::ZeroSizedLayout`] if the computed slice would be zero-sized.
	/// - [`AllocError::ShrinkBiggerNewLayout`] if `new_len > slice.len()`.
	///
	/// # Safety
	///
	/// - `ptr` must point to a slice allocated using this allocator.
	/// - `len` must describe exactly the number of initialized elements in that slice.
	unsafe fn realloc_raw_slice_zeroed<T>(
		&self,
		ptr: NonNull<T>,
		len: usize,
		new_len: usize,
	) -> Result<NonNull<T>, AllocError> {
		realloc!(ralloc, self, ptr, len, new_len, T, Zero)
	}

    /// Deallocates a previously allocated block.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a block of memory allocated using this allocator.
    /// - `n` must be the exact number of `T` held in that block.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn dealloc_n<T>(&self, ptr: NonNull<T>, n: usize) {
        dealloc_n(self, ptr, n);
    }

    /// Zeroes and deallocates `n` elements at the given pointer.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a block of memory allocated using this allocator, be valid for reads
    ///   and writes, aligned, and a valid `T`.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn zero_and_dealloc_n<T>(&self, ptr: NonNull<T>, n: usize) {
        ptr.as_ptr().write_bytes(0, n);
        self.dealloc_n(ptr, n);
    }

    /// Drops the data at a pointer and deallocates its previously allocated block.
    ///
    /// # Safety
    ///
    /// - `ptr` must point to a block of memory allocated using this allocator, be valid for reads
    ///   and writes, aligned, and contain `n` valid `T`.
    /// - `n` must be the exact number of `T` held in that block.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    unsafe fn drop_and_dealloc_n<T>(&self, ptr: NonNull<T>, n: usize) {
        NonNull::slice_from_raw_parts(ptr, n).drop_in_place();
        self.dealloc_n(ptr, n);
    }
}

impl<A: Alloc + ?Sized> AllocSlice for A {}

#[cfg(feature = "alloc_ext")]
/// Slice-specific extension methods for [`AllocExt`](crate::alloc_ext::AllocExt), providing
/// extended convenient functions for slice allocator operations.
pub trait AllocSliceExt: AllocSlice + crate::alloc_ext::AllocExt {
    /// Attempts to allocate a block of memory for `len` instances of `T`, filled with bytes
    /// initialized to `n`.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    fn alloc_slice_filled<T>(&self, len: usize, n: u8) -> Result<NonNull<[T]>, AllocError> {
        let layout = layout_or_sz_align::<T>(len)
            .map_err(|(sz, align)| AllocError::LayoutError(sz, align))?;
        self.alloc_filled(layout, n)
            .map(|ptr| NonNull::slice_from_raw_parts(ptr.cast(), len))
            .map_err(|_| AllocError::AllocFailed(layout))
    }

    /// Attempts to allocate a block of memory for `len` instances of `T` and
    /// fill it by calling `pattern(i)` for each byte index `i`.
    ///
    /// # Errors
    ///
    /// - [`AllocError::AllocFailed`] if allocation fails.
    /// - [`AllocError::LayoutError`] if the computed layout is invalid.
    /// - [`AllocError::ZeroSizedLayout`] if the computed slice has a size of zero.
    #[cfg_attr(miri, track_caller)]
    #[inline]
    fn alloc_slice_patterned<T, F: Fn(usize) -> u8 + Clone>(
        &self,
        len: usize,
        pattern: F,
    ) -> Result<NonNull<[T]>, AllocError> {
        let layout = layout_or_sz_align::<T>(len)
            .map_err(|(sz, align)| AllocError::LayoutError(sz, align))?;
        self.alloc_patterned(layout, pattern)
            .map(|ptr| NonNull::slice_from_raw_parts(ptr.cast(), len))
            .map_err(|_| AllocError::AllocFailed(layout))
    }
}

impl<A: AllocSlice + ?Sized> AllocSliceExt for A {}
