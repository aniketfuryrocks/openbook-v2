//! A central-limit order book (CLOB) program that targets the Sealevel runtime.

solana_program::declare_id!("opnb2LAfJYbRMAHHvqjCwQxanZn7ReEHp1k81EohpZb");

#[macro_use]
pub mod util;

pub mod accounts_zerocopy;
pub mod error;
pub mod logs;
pub mod pubkey_option;
pub mod state;
pub mod token_utils;
pub mod types;

mod i80f48;
