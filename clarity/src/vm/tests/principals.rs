use hashbrown::HashMap;
use stacks_common::types::StacksEpochId;
use stacks_common::util::hash::hex_bytes;

use crate::vm::ast::ASTRules;
use crate::vm::errors::CheckErrors;
use crate::vm::functions::principals::PrincipalConstructErrorCode;
use crate::vm::types::TypeSignature::PrincipalType;
use crate::vm::types::{
    ASCIIData, BuffData, CharType, OptionalData, PrincipalData, QualifiedContractIdentifier,
    ResponseData, SequenceData, StandardPrincipalData, TupleData, TypeSignature, Value, BUFF_1,
    BUFF_20,
};
use crate::vm::{execute_with_parameters, ClarityVersion};

#[test]
fn test_simple_is_standard_check_inputs() {
    let wrong_type_test = "(is-standard u10)";
    assert_eq!(
        execute_with_parameters(
            wrong_type_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap_err(),
        CheckErrors::TypeValueError(PrincipalType, Value::UInt(10)).into()
    );
}

#[test]
fn test_simple_is_standard_testnet_cases() {
    let testnet_addr_test = "(is-standard 'STB44HYPYAT2BB2QE513NSP81HTMYWBJP02HPGK6)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    let testnet_addr_test = "(is-standard 'STB44HYPYAT2BB2QE513NSP81HTMYWBJP02HPGK6.tokens)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    let testnet_addr_test = "(is-standard 'SN2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKP6D2ZK9)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    let testnet_addr_test = "(is-standard 'SN2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKP6D2ZK9.tokens)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            testnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
}

fn test_simple_is_standard_mainnet_cases() {
    let mainnet_addr_test = "(is-standard 'SP3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    let mainnet_addr_test = "(is-standard 'SP3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY.tokens)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    let mainnet_addr_test = "(is-standard 'SM3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    let mainnet_addr_test = "(is-standard 'SM3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY.tokens)";
    assert_eq!(
        Value::Bool(true),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            mainnet_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
}

#[test]
fn test_simple_is_standard_undefined_cases() {
    // When an address is neither a testnet nor a mainnet address, the result should be false.
    let invalid_addr_test = "(is-standard 'S1G2081040G2081040G2081040G208105NK8PE5)";
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            invalid_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            invalid_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    let invalid_addr_test = "(is-standard 'S1G2081040G2081040G2081040G208105NK8PE5.tokens)";
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            invalid_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        Value::Bool(false),
        execute_with_parameters(
            invalid_addr_test,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
}

/// Creates a Tuple which is the result of parsing a Principal tuple into a Tuple of its `version`
/// and `hash-bytes` and `name`
fn create_principal_destruct_tuple_from_strings(
    version: &str,
    hash_bytes: &str,
    name: Option<&str>,
) -> Value {
    Value::Tuple(
        TupleData::from_data(vec![
            (
                "version".into(),
                Value::Sequence(SequenceData::Buffer(BuffData {
                    data: hex_bytes(version).unwrap(),
                })),
            ),
            (
                "hash-bytes".into(),
                Value::Sequence(SequenceData::Buffer(BuffData {
                    data: hex_bytes(hash_bytes).unwrap(),
                })),
            ),
            (
                "name".into(),
                Value::Optional(OptionalData {
                    data: name.map(|name_str| {
                        Box::new(Value::Sequence(SequenceData::String(CharType::ASCII(
                            ASCIIData {
                                data: name_str.as_bytes().to_vec(),
                            },
                        ))))
                    }),
                }),
            ),
        ])
        .expect("FAIL: Failed to initialize tuple."),
    )
}

#[test]
// Test that we can parse well-formed principals.
fn test_principal_destruct_good() {
    // SP is mainnet single-sig. We run against mainnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'SP3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "16",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                None
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // SM is mainnet multi-sig. We run against mainnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'SM3X6QWWETNBZWGBK6DRGTR1KX50S74D341M9C5X7)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "14",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                None,
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // ST is testnet single-sig. We run against testnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'ST3X6QWWETNBZWGBK6DRGTR1KX50S74D3425Q1TPK)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "1a",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                None,
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // SN is testnet multi-sig. We run against testnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'SN3X6QWWETNBZWGBK6DRGTR1KX50S74D340JWTSC7)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "15",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                None
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // SP is mainnet single-sig. We run against mainnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'SP3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "16",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                Some("foo")
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // SM is mainnet multi-sig. We run against mainnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'SM3X6QWWETNBZWGBK6DRGTR1KX50S74D341M9C5X7.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "14",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                Some("foo")
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // ST is testnet single-sig. We run against testnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'ST3X6QWWETNBZWGBK6DRGTR1KX50S74D3425Q1TPK.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "1a",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                Some("foo")
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // SN is testnet multi-sig. We run against testnet so should get an `ok` value.
    let input = r#"(principal-destruct? 'SN3X6QWWETNBZWGBK6DRGTR1KX50S74D340JWTSC7.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "15",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                Some("foo")
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
}

#[test]
// Test that we notice principals that do not correspond to valid version bytes, and return them in
// the error channel.
fn test_principal_destruct_bad_version_byte() {
    // SZ is not a valid prefix for any Stacks network. But it's valid for the future.
    let input = r#"(principal-destruct? 'SZ2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQ9H6DPR)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "1f",
                "a46ff88886c2ef9762d970b4d2c63678835bd39d",
                None
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // SP is mainnet, but we run on testnet.
    let input = r#"(principal-destruct? 'SP3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "16",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                None
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // ST is testet, but we run on mainnet.
    let input = r#"(principal-destruct? 'ST3X6QWWETNBZWGBK6DRGTR1KX50S74D3425Q1TPK)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "1a",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                None
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // SZ is not a valid prefix for any Stacks network. But it's valid for the future.
    let input = r#"(principal-destruct? 'SZ2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQ9H6DPR.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "1f",
                "a46ff88886c2ef9762d970b4d2c63678835bd39d",
                Some("foo")
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // SP is mainnet, but we run on testnet.
    let input = r#"(principal-destruct? 'SP3X6QWWETNBZWGBK6DRGTR1KX50S74D3433WDGJY.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "16",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                Some("foo")
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // ST is testet, but we run on mainnet.
    let input = r#"(principal-destruct? 'ST3X6QWWETNBZWGBK6DRGTR1KX50S74D3425Q1TPK.foo)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(create_principal_destruct_tuple_from_strings(
                "1a",
                "fa6bf38ed557fe417333710d6033e9419391a320",
                Some("foo")
            ))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );
}

#[test]
// Standard case where construction should work.  We compare the output of the
// Clarity function to hand-built principals.
fn test_principal_construct_good() {
    // We always use the same bytes buffer.
    let mut transfer_buffer = [0u8; 20];
    transfer_buffer
        .copy_from_slice(&hex_bytes("fa6bf38ed557fe417333710d6033e9419391a320").unwrap());

    // Mainnet single-sig, on mainnet.
    let input = r#"(principal-construct? 0x16 0xfa6bf38ed557fe417333710d6033e9419391a320)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Standard(
                StandardPrincipalData(22, transfer_buffer)
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // Mainnet multi-sig, on mainnet.
    let input = r#"(principal-construct? 0x14 0xfa6bf38ed557fe417333710d6033e9419391a320)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Standard(
                StandardPrincipalData(20, transfer_buffer)
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // Mainnet single-sig contract, on mainnet.
    let input =
        r#"(principal-construct? 0x16 0xfa6bf38ed557fe417333710d6033e9419391a320 "hello-world")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Contract(
                QualifiedContractIdentifier::new(
                    StandardPrincipalData(22, transfer_buffer),
                    "hello-world".into()
                )
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // Mainnet multi-sig contract, on mainnet.
    let input =
        r#"(principal-construct? 0x14 0xfa6bf38ed557fe417333710d6033e9419391a320 "hello-world")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Contract(
                QualifiedContractIdentifier::new(
                    StandardPrincipalData(20, transfer_buffer),
                    "hello-world".into()
                )
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            true
        )
        .unwrap()
        .unwrap()
    );

    // Testnet single-sig, run on testnet.
    let input = r#"(principal-construct? 0x1a 0xfa6bf38ed557fe417333710d6033e9419391a320)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Standard(
                StandardPrincipalData(26, transfer_buffer)
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // Testnet multi-sig, run on testnet.
    let input = r#"(principal-construct? 0x15 0xfa6bf38ed557fe417333710d6033e9419391a320)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Standard(
                StandardPrincipalData(21, transfer_buffer)
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // Testnet single-sig contract, run on testnet.
    let input =
        r#"(principal-construct? 0x1a 0xfa6bf38ed557fe417333710d6033e9419391a320 "hello-world")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Contract(
                QualifiedContractIdentifier::new(
                    StandardPrincipalData(26, transfer_buffer),
                    "hello-world".into()
                )
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // Testnet multi-sig contract, run on testnet.
    let input =
        r#"(principal-construct? 0x15 0xfa6bf38ed557fe417333710d6033e9419391a320 "hello-world")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: true,
            data: Box::new(Value::Principal(PrincipalData::Contract(
                QualifiedContractIdentifier::new(
                    StandardPrincipalData(21, transfer_buffer),
                    "hello-world".into()
                )
            )))
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
}

/// Creates a `Principal`-type `Value` from string-based byte representations.
fn create_principal_from_strings(
    version_string: &str,
    principal_string: &str,
    name: Option<&str>,
) -> Value {
    let mut version_array = [0u8; 1];
    version_array.copy_from_slice(&hex_bytes(version_string).expect("hex_arrays failed"));
    let mut principal_array = [0u8; 20];
    principal_array.copy_from_slice(&hex_bytes(principal_string).expect("hex_bytes failed"));

    if let Some(name) = name {
        // contract principal requested
        Value::Principal(PrincipalData::Contract(QualifiedContractIdentifier::new(
            StandardPrincipalData(version_array[0], principal_array),
            name.into(),
        )))
    } else {
        // standard principal requested
        Value::Principal(PrincipalData::Standard(StandardPrincipalData(
            version_array[0],
            principal_array,
        )))
    }
}

#[test]
// Test cases where the version byte is of the right type `(buff 1)`, but where the byte doesn't
// match a recognized network. This is meant for compatibility with "future" network bytes, so
// is still valid.
fn test_principal_construct_version_byte_future() {
    // The version byte 0x1f is unrecognized today, but is valid for the future.
    let input = r#"(principal-construct? 0x1f 0x0102030405060708091011121314151617181920)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::VERSION_BYTE as u128)
                    ),
                    (
                        "value".into(),
                        Value::some(create_principal_from_strings(
                            "1f",
                            "0102030405060708091011121314151617181920",
                            None
                        ))
                        .expect("Value::some failed.")
                    ),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // The version byte 0x1f is unrecognized today, but is valid for the future.
    let input =
        r#"(principal-construct? 0x1f 0x0102030405060708091011121314151617181920 "hello-world")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::VERSION_BYTE as u128)
                    ),
                    (
                        "value".into(),
                        Value::some(create_principal_from_strings(
                            "1f",
                            "0102030405060708091011121314151617181920",
                            Some("hello-world")
                        ))
                        .expect("Value::some failed.")
                    ),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
}

#[test]
// Test cases where the wrong type should be a `CheckErrors` error, because it should have been
// caught by the type checker.
fn test_principal_construct_check_errors() {
    // The version bytes 0x5904934 are invalid. Should have been caught by type checker so use
    // `CheckErrors`.
    let input = r#"(principal-construct? 0x590493 0x0102030405060708091011121314151617181920)"#;
    assert_eq!(
        Err(CheckErrors::TypeValueError(
            BUFF_1.clone(),
            Value::Sequence(SequenceData::Buffer(BuffData {
                data: hex_bytes("590493").unwrap()
            }))
        )
        .into()),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
    );

    // u22 is not a byte buffer, so is invalid. Should have been caught by type checker so use
    // `CheckErrors`.
    let input = r#"(principal-construct? u22 0x0102030405060708091011121314151617181920)"#;
    assert_eq!(
        Err(CheckErrors::TypeValueError(BUFF_1.clone(), Value::UInt(22)).into()),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
    );

    // Hash key part is too large, should have length 20. This is a `CheckErrors` error because it
    // should have been caught by the type checker.
    let input = r#"(principal-construct? 0x16 0x010203040506070809101112131415161718192021)"#;
    assert_eq!(
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap_err(),
        CheckErrors::TypeValueError(
            BUFF_20.clone(),
            Value::Sequence(SequenceData::Buffer(BuffData {
                data: hex_bytes("010203040506070809101112131415161718192021").unwrap()
            }))
        )
        .into()
    );

    // Name is too long, which should have been caught by the type-checker
    let input = r#"(principal-construct? 0x16 0x0102030405060708091011121314151617181920 "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")"#;
    assert_eq!(
        Err(CheckErrors::TypeValueError(
            TypeSignature::contract_name_string_ascii_type().unwrap(),
            Value::Sequence(SequenceData::String(CharType::ASCII(ASCIIData {
                data: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
                    .as_bytes()
                    .to_vec()
            })))
        )
        .into()),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
    );
}

#[test]
// Test cases where we return an "in response" error.
fn test_principal_construct_response_errors() {
    // Hash key part is too small, should have length 20. This wasn't for the type checker, so the
    // error is signaled in the returned Response.
    let input = r#"(principal-construct? 0x16 0x01020304050607080910111213141516171819)"#;
    assert_eq!(
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap(),
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::BUFFER_LENGTH as u128)
                    ),
                    ("value".into(), Value::none()),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
    );

    // Version byte is too small, should have length 1. This error is signaled in the returned
    // Response.
    let input = r#"(principal-construct? 0x 0x0102030405060708091011121314151617181920)"#;
    assert_eq!(
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap(),
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::BUFFER_LENGTH as u128)
                    ),
                    ("value".into(), Value::none()),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
    );

    // The version byte 0x20 is too big, even for the future. So, we get no result.
    let input = r#"(principal-construct? 0x20 0x0102030405060708091011121314151617181920)"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::BUFFER_LENGTH as u128)
                    ),
                    ("value".into(), Value::none()),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // The contract name is too short
    let input = r#"(principal-construct? 0x16 0x0102030405060708091011121314151617181920 "")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::CONTRACT_NAME as u128)
                    ),
                    ("value".into(), Value::none()),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );

    // The contract name is not a valid contract name
    let input = r#"(principal-construct? 0x16 0x0102030405060708091011121314151617181920 "foo[")"#;
    assert_eq!(
        Value::Response(ResponseData {
            committed: false,
            data: Box::new(Value::Tuple(
                TupleData::from_data(vec![
                    (
                        "error_code".into(),
                        Value::UInt(PrincipalConstructErrorCode::CONTRACT_NAME as u128)
                    ),
                    ("value".into(), Value::none()),
                ])
                .expect("FAIL: Failed to initialize tuple."),
            )),
        }),
        execute_with_parameters(
            input,
            ClarityVersion::Clarity2,
            StacksEpochId::Epoch21,
            ASTRules::PrecheckSize,
            false
        )
        .unwrap()
        .unwrap()
    );
}
