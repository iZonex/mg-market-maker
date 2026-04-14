//! FIX 4.4 standard tag numbers used by this crate.
//!
//! Full spec: <https://www.fixtrading.org/standards/fix-4-4/>

// --- Session / header ---
pub const BEGIN_STRING: u32 = 8;
pub const BODY_LENGTH: u32 = 9;
pub const CHECKSUM: u32 = 10;
pub const MSG_SEQ_NUM: u32 = 34;
pub const MSG_TYPE: u32 = 35;
pub const SENDER_COMP_ID: u32 = 49;
pub const SENDING_TIME: u32 = 52;
pub const TARGET_COMP_ID: u32 = 56;
pub const TEST_REQ_ID: u32 = 112;

// --- Order entry ---
pub const CL_ORD_ID: u32 = 11;
pub const ORIG_CL_ORD_ID: u32 = 41;
pub const ORDER_QTY: u32 = 38;
pub const ORD_TYPE: u32 = 40;
pub const PRICE: u32 = 44;
pub const SIDE: u32 = 54;
pub const SYMBOL: u32 = 55;
pub const TIME_IN_FORCE: u32 = 59;
pub const TRANSACT_TIME: u32 = 60;

// --- Logon ---
pub const ENCRYPT_METHOD: u32 = 98;
pub const HEART_BT_INT: u32 = 108;
