use crate::Account;

pub(super) fn account<'a>(address: &'a str, owner: &'a str, data: &'a [u8]) -> Account<'a> {
    Account {
        address,
        owner,
        data,
        executable: false,
        lamports: 1,
    }
}

pub(super) fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
