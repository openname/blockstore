use clarity::vm::contexts::OwnedEnvironment;
use clarity::vm::errors::{Error, RuntimeErrorType};
use clarity::vm::test_util::{TEST_BURN_STATE_DB, TEST_HEADER_DB};
use clarity::vm::types::QualifiedContractIdentifier;
use stacks_common::consts::{FIRST_BURNCHAIN_CONSENSUS_HASH, FIRST_STACKS_BLOCK_HASH};
use stacks_common::types::chainstate::{BlockHeaderHash, StacksBlockId};
use stacks_common::types::StacksEpochId;

use crate::chainstate::stacks::index::ClarityMarfTrieId;
use crate::clarity_vm::database::marf::MarfedKV;

pub fn with_marfed_environment<F>(f: F, top_level: bool)
where
    F: FnOnce(&mut OwnedEnvironment),
{
    let mut marf_kv = MarfedKV::temporary();

    {
        let mut store = marf_kv.begin(
            &StacksBlockId::sentinel(),
            &StacksBlockId::new(&FIRST_BURNCHAIN_CONSENSUS_HASH, &FIRST_STACKS_BLOCK_HASH),
        );

        store
            .as_clarity_db(&TEST_HEADER_DB, &TEST_BURN_STATE_DB)
            .initialize();
        store.test_commit();
    }

    {
        let mut store = marf_kv.begin(
            &StacksBlockId::new(&FIRST_BURNCHAIN_CONSENSUS_HASH, &FIRST_STACKS_BLOCK_HASH),
            &StacksBlockId([1; 32]),
        );

        let mut owned_env = OwnedEnvironment::new(
            store.as_clarity_db(&TEST_HEADER_DB, &TEST_BURN_STATE_DB),
            StacksEpochId::latest(),
        );
        // start an initial transaction.
        if !top_level {
            owned_env.begin();
        }

        f(&mut owned_env)
    }
}

#[test]
fn test_at_unknown_block() {
    fn test(owned_env: &mut OwnedEnvironment) {
        let contract = "(define-data-var foo int 3)
                        (at-block 0x0202020202020202020202020202020202020202020202020202020202020202
                          (+ 1 2))";
        let err = owned_env
            .initialize_contract(
                QualifiedContractIdentifier::local("contract").unwrap(),
                contract,
                None,
                clarity::vm::ast::ASTRules::PrecheckSize,
            )
            .unwrap_err();
        eprintln!("{}", err);
        match err {
            Error::Runtime(x, _) => assert_eq!(
                x,
                RuntimeErrorType::UnknownBlockHeaderHash(BlockHeaderHash::from(
                    vec![2; 32].as_slice()
                ))
            ),
            _ => panic!("Unexpected error"),
        }
    }

    with_marfed_environment(test, true);
}
