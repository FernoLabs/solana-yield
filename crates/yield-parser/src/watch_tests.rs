use super::run;

fn parse_output(
    input: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let bytes = serde_json::to_vec(input)?;
    let mut output = Vec::new();
    run(bytes.as_slice(), &mut output)?;
    Ok(serde_json::from_slice(&output)?)
}

#[test]
fn watch_list_matches_the_existing_29_product_33_key_profile() {
    let mut markets = Vec::new();
    for fixture in [
        &include_bytes!("../fixtures/live-save.json")[..],
        &include_bytes!("../fixtures/live-project0.json")[..],
        &include_bytes!("../fixtures/live-vaults.json")[..],
    ] {
        markets.extend(serde_json::from_slice::<Vec<serde_json::Value>>(fixture).unwrap());
    }
    assert_eq!(markets.len(), 29);
    markets.reverse();
    let output = parse_output(&serde_json::json!({"watch_list":{"markets":markets}})).unwrap();
    let expected: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/live-watch-keys.json")).unwrap();
    assert_eq!(expected.as_array().unwrap().len(), 33);
    assert_eq!(
        output,
        serde_json::json!({"schema_version":1,"keys":expected})
    );
}

fn watch_market(id: &str, address: &str) -> serde_json::Value {
    serde_json::json!({"id":id,"kind":"save","accounts":{
        "reserve":address,
        "lending_market":"7XttJ7hp83u5euzT7ybC5zsjdgKA4WPbQHVS27CATAJH",
        "asset_mint":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    }})
}

#[test]
fn watch_list_uses_kamino_and_jupiter_adapter_requirements() {
    const RESERVE: &str = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp";
    const LENDING: &str = "2vVYHYM8VYnvZqQWpTJSj8o8DBf1wM8pVs3bsTgYZiqJ";
    let source: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/kamino-reserve-usdg.json")).unwrap();
    let input = serde_json::json!({"watch_list":{"markets":[
        {"id":"kamino","kind":"kamino_reserve","accounts":{
            "reserve":RESERVE,
            "lending_market":source["parser"]["lending_market"],
            "asset_mint":source["parser"]["asset_mint"],
            "share_mint":source["parser"]["share_mint"]
        }},
        {"id":"jupiter","kind":"jupiter_earn","accounts":{
            "lending":LENDING,
            "liquidity_reserve":"94vK29npVbyRHXH63rRcTiSr26SFhrQTzbpNJuhQEDu",
            "asset_mint":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "share_mint":"9BEcn9aPEmhSPbPQeFGjidRiEKki46fVQDyPpSQXPA2D"
        }}
    ]}});
    let mut expected = vec![RESERVE, LENDING, yield_parser::live::CLOCK];
    expected.sort();
    assert_eq!(
        parse_output(&input).unwrap(),
        serde_json::json!({"schema_version":1,"keys":expected})
    );
}

#[test]
fn watch_list_deduplicates_addresses_and_rejects_bad_configuration() {
    const ADDRESS: &str = "BJfY2E6TVEQe896kfR1AhhYCECVKfbceGg2ASU2LXdiA";
    let market = watch_market("save", ADDRESS);
    let output = parse_output(&serde_json::json!({"watch_list":{"markets":[
        market.clone(),watch_market("same-reserve", ADDRESS)
    ]}}))
    .unwrap();
    assert_eq!(output["keys"].as_array().unwrap().len(), 2);
    for input in [
        serde_json::json!({"watch_list":{"markets":[]}}),
        serde_json::json!({"watch_list":{"markets":[market.clone(),market.clone()]}}),
        serde_json::json!({"watch_list":{"markets":[watch_market(" ",ADDRESS)]}}),
        serde_json::json!({"watch_list":{"markets":[watch_market(&"a".repeat(161),ADDRESS)]}}),
        serde_json::json!({"watch_list":{"markets":[watch_market("save","invalid-key")]}}),
        serde_json::json!({"watch_list":{"markets":[{"id":"unknown","kind":"unknown","accounts":{}}]}}),
        serde_json::json!({"watch_list":{"markets":[{"id":"missing","kind":"save","accounts":{}}]}}),
        serde_json::json!({"watch_list":{"markets":[market.clone()],"unexpected":true}}),
    ] {
        assert!(parse_output(&input).is_err(), "accepted {input}");
    }

    let too_many_markets = (0..101)
        .map(|id| watch_market(&format!("save-{id}"), ADDRESS))
        .collect::<Vec<_>>();
    assert!(parse_output(&serde_json::json!({"watch_list":{"markets":too_many_markets}})).is_err());
    let too_many_keys = (0..100_u8)
        .map(|id| {
            let key = bs58::encode([id + 1; 32]).into_string();
            watch_market(&format!("save-{id}"), &key)
        })
        .collect::<Vec<_>>();
    assert!(parse_output(&serde_json::json!({"watch_list":{"markets":too_many_keys}})).is_err());
}
