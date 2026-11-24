use yield_parser::{
    adapter::{
        observe, AccountProvider, AccountRequirement, AccountSet, ObservationContext,
        ProtocolAdapter, MAX_ACCOUNTS,
    },
    live::{Measure, CLOCK},
    Account, ParseError,
};

const POOL: &str = "2vVYHYM8VYnvZqQWpTJSj8o8DBf1wM8pVs3bsTgYZiqJ";
const OTHER_POOL: &str = "94vK29npVbyRHXH63rRcTiSr26SFhrQTzbpNJuhQEDu";
const OWNER: &str = "jup3YeL8QhtSx1e253b2FDvsMNC87fDrgQZivbrndc9";
const SYSVAR: &str = "Sysvar1111111111111111111111111111111111111";

// This adapter lives outside the parser library and needs no protocol enum edit.
struct ExternalAdapter {
    requirements: Vec<AccountRequirement<'static>>,
    values: Vec<Measure>,
}

impl ExternalAdapter {
    fn new() -> Self {
        Self {
            requirements: vec![AccountRequirement {
                role: "pool",
                address: POOL,
            }],
            values: vec![Measure {
                kind: "stored_assets_per_share",
                numerator: 0,
                denominator: 10,
                updated_slot: None,
                updated_unix_seconds: None,
            }],
        }
    }
}

impl ProtocolAdapter for ExternalAdapter {
    fn protocol(&self) -> &'static str {
        "external_example"
    }

    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        self.requirements.clone()
    }

    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        if context.account(POOL)?.owner != OWNER {
            return Err(ParseError::InvalidAccount);
        }
        Ok(self.values.clone())
    }
}

fn clock_data(slot: u64, time: i64) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..8].copy_from_slice(&slot.to_le_bytes());
    bytes[32..40].copy_from_slice(&time.to_le_bytes());
    bytes
}

fn clock(data: &[u8]) -> Account<'_> {
    Account {
        address: CLOCK,
        owner: SYSVAR,
        executable: false,
        lamports: 1,
        data,
    }
}

fn pool() -> Account<'static> {
    Account {
        address: POOL,
        owner: OWNER,
        executable: false,
        lamports: 1,
        data: &[],
    }
}

#[test]
fn external_object_safe_adapter_preserves_zero_and_absent_update_fields() {
    let clock = clock_data(8, 1_000);
    let records = [self::clock(&clock), pool()];
    let accounts = AccountSet::new(&records).unwrap();
    let context = ObservationContext::new(8, &accounts).unwrap();
    let adapter: Box<dyn ProtocolAdapter> = Box::new(ExternalAdapter::new());
    assert_eq!(context.slot(), 8);
    assert_eq!(context.unix_seconds(), 1_000);
    assert_eq!(
        observe(adapter.as_ref(), &context).unwrap(),
        ExternalAdapter::new().values
    );
}

#[test]
fn invalid_or_missing_clock_rejects_the_context() {
    let records = [pool()];
    let accounts = AccountSet::new(&records).unwrap();
    assert!(ObservationContext::new(8, &accounts).is_err());
    for (slot, time) in [(9, 1_000), (8, -1), (8, 0)] {
        let data = clock_data(slot, time);
        let records = [clock(&data), pool()];
        let accounts = AccountSet::new(&records).unwrap();
        assert!(ObservationContext::new(8, &accounts).is_err());
    }
}

#[test]
fn missing_and_uninitialized_required_accounts_are_rejected() {
    let data = clock_data(8, 1_000);
    let records = [clock(&data)];
    let accounts = AccountSet::new(&records).unwrap();
    let context = ObservationContext::new(8, &accounts).unwrap();
    assert!(observe(&ExternalAdapter::new(), &context).is_err());
    for bad in [
        Account {
            lamports: 0,
            ..pool()
        },
        Account {
            executable: true,
            ..pool()
        },
        Account {
            owner: "not-a-key",
            ..pool()
        },
    ] {
        let records = [clock(&data), bad];
        let accounts = AccountSet::new(&records).unwrap();
        let context = ObservationContext::new(8, &accounts).unwrap();
        assert!(observe(&ExternalAdapter::new(), &context).is_err());
    }
}

struct SubstitutingProvider<'a> {
    clock: Account<'a>,
}

impl AccountProvider for SubstitutingProvider<'_> {
    fn account(&self, address: &str) -> Result<Account<'_>, ParseError> {
        if address == CLOCK {
            Ok(self.clock)
        } else {
            Ok(Account {
                address: OTHER_POOL,
                ..pool()
            })
        }
    }
}

#[test]
fn a_provider_cannot_substitute_a_different_valid_account_address() {
    let data = clock_data(8, 1_000);
    let provider = SubstitutingProvider {
        clock: clock(&data),
    };
    let context = ObservationContext::new(8, &provider).unwrap();
    assert_eq!(
        observe(&ExternalAdapter::new(), &context),
        Err(ParseError::AccountMismatch)
    );
}

#[test]
fn invalid_measurements_and_future_protocol_updates_are_rejected() {
    let data = clock_data(8, 1_000);
    let records = [clock(&data), pool()];
    let accounts = AccountSet::new(&records).unwrap();
    let context = ObservationContext::new(8, &accounts).unwrap();
    let valid = ExternalAdapter::new().values[0];
    for values in [
        vec![],
        vec![Measure {
            denominator: 0,
            ..valid
        }],
        vec![Measure {
            updated_slot: Some(9),
            ..valid
        }],
        vec![Measure {
            updated_unix_seconds: Some(1_001),
            ..valid
        }],
        vec![valid, valid],
    ] {
        let adapter = ExternalAdapter {
            values,
            ..ExternalAdapter::new()
        };
        assert_eq!(observe(&adapter, &context), Err(ParseError::InvalidValue));
    }
    let adapter = ExternalAdapter {
        values: vec![Measure {
            updated_slot: Some(7),
            updated_unix_seconds: Some(900),
            ..valid
        }],
        ..ExternalAdapter::new()
    };
    assert_eq!(observe(&adapter, &context).unwrap(), adapter.values);
}

#[test]
fn duplicate_roles_fail_but_distinct_roles_can_read_the_same_address() {
    let data = clock_data(8, 1_000);
    let records = [clock(&data), pool()];
    let accounts = AccountSet::new(&records).unwrap();
    let context = ObservationContext::new(8, &accounts).unwrap();
    let mut adapter = ExternalAdapter::new();
    adapter.requirements.push(adapter.requirements[0]);
    assert_eq!(
        observe(&adapter, &context),
        Err(ParseError::AccountMismatch)
    );
    adapter.requirements[1].role = "other_role";
    assert!(observe(&adapter, &context).is_ok());
}

#[test]
fn account_sets_are_bounded_and_do_not_allow_duplicate_keys() {
    assert!(AccountSet::new(&[]).is_err());
    assert!(AccountSet::new(&vec![pool(); MAX_ACCOUNTS + 1]).is_err());
    assert!(AccountSet::new(&[pool(), pool()]).is_err());
}

struct UndeclaredReader;

impl ProtocolAdapter for UndeclaredReader {
    fn protocol(&self) -> &'static str {
        "undeclared_reader"
    }

    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![AccountRequirement {
            role: "pool",
            address: POOL,
        }]
    }

    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        context.account(OTHER_POOL)?;
        Ok(ExternalAdapter::new().values)
    }
}

struct CountingProvider<'a> {
    records: AccountSet<'a>,
    undeclared_reads: std::cell::Cell<usize>,
}

impl AccountProvider for CountingProvider<'_> {
    fn account(&self, address: &str) -> Result<Account<'_>, ParseError> {
        if address == OTHER_POOL {
            self.undeclared_reads.set(self.undeclared_reads.get() + 1);
        }
        self.records.account(address)
    }
}

#[test]
fn undeclared_account_reads_fail_before_the_provider_is_queried() {
    let data = clock_data(8, 1_000);
    let records = [
        clock(&data),
        pool(),
        Account {
            address: OTHER_POOL,
            ..pool()
        },
    ];
    let provider = CountingProvider {
        records: AccountSet::new(&records).unwrap(),
        undeclared_reads: std::cell::Cell::new(0),
    };
    let context = ObservationContext::new(8, &provider).unwrap();
    assert_eq!(
        observe(&UndeclaredReader, &context),
        Err(ParseError::AccountMismatch)
    );
    assert_eq!(provider.undeclared_reads.get(), 0);
}
