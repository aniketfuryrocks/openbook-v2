use anchor_lang::prelude::*;
/// Nothing in Rust shall use these types. They only exist so that the Anchor IDL
/// knows about them and typescript can deserialize it.

#[derive(AnchorSerialize, AnchorDeserialize, Default)]
pub struct I80F48 {
    val: i128,
}

/// A 128-bit unsigned integer.
/// This is a workaround for the fact the rust changed
/// the alignment of u128 from 8 to 16 bytes.
#[derive(Copy, Clone, bytemuck::Zeroable, bytemuck::Pod, Debug)]
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct aligned_u128(pub [u8; 16]);

impl From<aligned_u128> for u128 {
    fn from(val: aligned_u128) -> Self {
        u128::from_le_bytes(val.0)
    }
}

impl From<u128> for aligned_u128 {
    fn from(val: u128) -> Self {
        aligned_u128(val.to_le_bytes())
    }
}

/// A 128-bit signed integer.
/// This is a workaround for the fact the rust changed
/// the alignment of i128 from 8 to 16 bytes.
#[derive(Copy, Clone, bytemuck::Zeroable, bytemuck::Pod, Debug)]
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct aligned_i128(pub [u8; 16]);

impl From<aligned_i128> for i128 {
    fn from(val: aligned_i128) -> Self {
        i128::from_le_bytes(val.0)
    }
}

impl From<i128> for aligned_i128 {
    fn from(val: i128) -> Self {
        aligned_i128(val.to_le_bytes())
    }
}
