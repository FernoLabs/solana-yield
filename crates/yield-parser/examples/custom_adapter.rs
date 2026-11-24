//! A synthetic, offline example of an adapter defined outside the parser.
//! Replace the layout, owner, units, and identity checks for a real protocol.

use yield_parser::{
    adapter::{self, AccountRequirement, AccountSet, ObservationContext, ProtocolAdapter},
    live::{Measure, CLOCK},
    Account, ParseError,
};

struct ExampleVault<'a> {
    address: &'a str,
    owner: &'a str,
}

impl ProtocolAdapter for ExampleVault<'_> {
    fn protocol(&self) -> &'static str {
        "example_vault"
    }

    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![AccountRequirement {
            role: "vault",
            address: self.address,
        }]
    }

    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        let account = context.account(self.address)?;
        if account.owner != self.owner {
            return Err(ParseError::InvalidAccount);
        }
        if account.data.len() != 24 || &account.data[..8] != b"EXAMPLE1" {
            return Err(ParseError::UnsupportedLayout);
        }
        // The length and discriminator have been checked before these reads.
        let assets = u64::from_le_bytes(account.data[8..16].try_into().unwrap());
        let shares = u64::from_le_bytes(account.data[16..24].try_into().unwrap());
        Ok(vec![Measure {
            kind: "share_value",
            numerator: u128::from(assets),
            denominator: u128::from(shares),
            updated_slot: None,
            updated_unix_seconds: None,
        }])
    }
}

fn main() -> Result<(), ParseError> {
    let slot = 100;
    let mut clock = [0_u8; 40];
    clock[..8].copy_from_slice(&u64::to_le_bytes(slot));
    clock[32..].copy_from_slice(&1_700_000_000_i64.to_le_bytes());
    let mut data = Vec::from(&b"EXAMPLE1"[..]);
    data.extend_from_slice(&1_050_u64.to_le_bytes());
    data.extend_from_slice(&1_000_u64.to_le_bytes());
    let address = bs58::encode([42; 32]).into_string();
    let owner = bs58::encode([43; 32]).into_string();
    let accounts = [
        Account {
            address: CLOCK,
            owner: "Sysvar1111111111111111111111111111111111111",
            executable: false,
            lamports: 1,
            data: &clock,
        },
        Account {
            address: &address,
            owner: &owner,
            executable: false,
            lamports: 1,
            data: &data,
        },
    ];
    let snapshot = AccountSet::new(&accounts)?;
    let context = ObservationContext::new(slot, &snapshot)?;
    let values = adapter::observe(
        &ExampleVault {
            address: &address,
            owner: &owner,
        },
        &context,
    )?;
    assert_eq!(values[0].numerator, 1_050);
    assert_eq!(values[0].denominator, 1_000);
    println!("synthetic share value: 1050 / 1000");
    Ok(())
}
