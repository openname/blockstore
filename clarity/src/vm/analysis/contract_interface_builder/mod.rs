// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::collections::{BTreeMap, BTreeSet};

use stacks_common::types::StacksEpochId;

use crate::vm::analysis::types::ContractAnalysis;
use crate::vm::analysis::CheckResult;
use crate::vm::types::signatures::CallableSubtype;
use crate::vm::types::{
    FixedFunction, FunctionArg, FunctionType, TupleTypeSignature, TypeSignature,
};
use crate::vm::{CheckErrors, ClarityName, ClarityVersion};

pub fn build_contract_interface(
    contract_analysis: &ContractAnalysis,
) -> CheckResult<ContractInterface> {
    let mut contract_interface =
        ContractInterface::new(contract_analysis.epoch, contract_analysis.clarity_version);

    let ContractAnalysis {
        private_function_types,
        public_function_types,
        read_only_function_types,
        variable_types,
        persisted_variable_types,
        map_types,
        fungible_tokens,
        non_fungible_tokens,
        epoch: _,
        clarity_version: _,
        defined_traits: _,
        implemented_traits: _,
        expressions: _,
        contract_identifier: _,
        type_map: _,
        cost_track: _,
        contract_interface: _,
        is_cost_contract_eligible: _,
    } = contract_analysis;

    contract_interface
        .functions
        .append(&mut ContractInterfaceFunction::from_map(
            private_function_types,
            ContractInterfaceFunctionAccess::private,
        )?);

    contract_interface
        .functions
        .append(&mut ContractInterfaceFunction::from_map(
            public_function_types,
            ContractInterfaceFunctionAccess::public,
        )?);

    contract_interface
        .functions
        .append(&mut ContractInterfaceFunction::from_map(
            read_only_function_types,
            ContractInterfaceFunctionAccess::read_only,
        )?);

    contract_interface
        .variables
        .append(&mut ContractInterfaceVariable::from_map(
            variable_types,
            ContractInterfaceVariableAccess::constant,
        ));

    contract_interface
        .variables
        .append(&mut ContractInterfaceVariable::from_map(
            persisted_variable_types,
            ContractInterfaceVariableAccess::variable,
        ));

    contract_interface
        .maps
        .append(&mut ContractInterfaceMap::from_map(map_types));

    contract_interface.non_fungible_tokens.append(
        &mut ContractInterfaceNonFungibleTokens::from_map(non_fungible_tokens),
    );

    contract_interface
        .fungible_tokens
        .append(&mut ContractInterfaceFungibleTokens::from_set(
            fungible_tokens,
        ));

    Ok(contract_interface)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContractInterfaceFunctionAccess {
    private,
    public,
    read_only,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceTupleEntryType {
    pub name: String,
    #[serde(rename = "type")]
    pub type_f: ContractInterfaceAtomType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContractInterfaceAtomType {
    none,
    int128,
    uint128,
    bool,
    principal,
    buffer {
        length: u32,
    },
    #[serde(rename = "string-utf8")]
    string_utf8 {
        length: u32,
    },
    #[serde(rename = "string-ascii")]
    string_ascii {
        length: u32,
    },
    tuple(Vec<ContractInterfaceTupleEntryType>),
    optional(Box<ContractInterfaceAtomType>),
    response {
        ok: Box<ContractInterfaceAtomType>,
        error: Box<ContractInterfaceAtomType>,
    },
    list {
        #[serde(rename = "type")]
        type_f: Box<ContractInterfaceAtomType>,
        length: u32,
    },
    trait_reference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceFungibleTokens {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceNonFungibleTokens {
    pub name: String,
    #[serde(rename = "type")]
    pub type_f: ContractInterfaceAtomType,
}

impl ContractInterfaceAtomType {
    pub fn from_tuple_type(tuple_type: &TupleTypeSignature) -> ContractInterfaceAtomType {
        ContractInterfaceAtomType::tuple(Self::vec_from_tuple_type(tuple_type))
    }

    pub fn vec_from_tuple_type(
        tuple_type: &TupleTypeSignature,
    ) -> Vec<ContractInterfaceTupleEntryType> {
        let mut out: Vec<_> = tuple_type
            .get_type_map()
            .iter()
            .map(|(name, sig)| ContractInterfaceTupleEntryType {
                name: name.to_string(),
                type_f: Self::from_type_signature(sig),
            })
            .collect();
        out.sort_unstable_by(|ty1, ty2| ty1.name.cmp(&ty2.name));
        out
    }

    pub fn from_type_signature(sig: &TypeSignature) -> ContractInterfaceAtomType {
        use crate::vm::types::SequenceSubtype::*;
        use crate::vm::types::StringSubtype::*;
        use crate::vm::types::TypeSignature::*;

        match sig {
            NoType => ContractInterfaceAtomType::none,
            IntType => ContractInterfaceAtomType::int128,
            UIntType => ContractInterfaceAtomType::uint128,
            BoolType => ContractInterfaceAtomType::bool,
            PrincipalType => ContractInterfaceAtomType::principal,
            CallableType(CallableSubtype::Principal(_)) => ContractInterfaceAtomType::principal,
            CallableType(CallableSubtype::Trait(_)) | TraitReferenceType(_) => {
                ContractInterfaceAtomType::trait_reference
            }
            ListUnionType(_) => ContractInterfaceAtomType::principal,
            TupleType(sig) => ContractInterfaceAtomType::from_tuple_type(sig),
            SequenceType(StringType(ASCII(len))) => {
                ContractInterfaceAtomType::string_ascii { length: len.into() }
            }
            SequenceType(StringType(UTF8(len))) => {
                ContractInterfaceAtomType::string_utf8 { length: len.into() }
            }
            SequenceType(BufferType(len)) => {
                ContractInterfaceAtomType::buffer { length: len.into() }
            }
            SequenceType(ListType(list_data)) => {
                let (type_f, length) = list_data.clone().destruct();
                ContractInterfaceAtomType::list {
                    type_f: Box::new(Self::from_type_signature(&type_f)),
                    length,
                }
            }
            OptionalType(sig) => {
                ContractInterfaceAtomType::optional(Box::new(Self::from_type_signature(sig)))
            }
            TypeSignature::ResponseType(boxed_sig) => {
                let (ok_sig, err_sig) = boxed_sig.as_ref();
                ContractInterfaceAtomType::response {
                    ok: Box::new(Self::from_type_signature(ok_sig)),
                    error: Box::new(Self::from_type_signature(err_sig)),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceFunctionArg {
    pub name: String,
    #[serde(rename = "type")]
    pub type_f: ContractInterfaceAtomType,
}

impl ContractInterfaceFunctionArg {
    pub fn from_function_args(fnArgs: &[FunctionArg]) -> Vec<ContractInterfaceFunctionArg> {
        fnArgs
            .iter()
            .map(|fnArg| ContractInterfaceFunctionArg {
                name: fnArg.name.to_string(),
                type_f: ContractInterfaceAtomType::from_type_signature(&fnArg.signature),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceFunctionOutput {
    #[serde(rename = "type")]
    pub type_f: ContractInterfaceAtomType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceFunction {
    pub name: String,
    pub access: ContractInterfaceFunctionAccess,
    pub args: Vec<ContractInterfaceFunctionArg>,
    pub outputs: ContractInterfaceFunctionOutput,
}

impl ContractInterfaceFunction {
    fn from_map(
        map: &BTreeMap<ClarityName, FunctionType>,
        access: ContractInterfaceFunctionAccess,
    ) -> CheckResult<Vec<ContractInterfaceFunction>> {
        map.iter()
            .map(|(name, function_type)| {
                Ok(ContractInterfaceFunction {
                    name: name.clone().into(),
                    access: access.to_owned(),
                    outputs: ContractInterfaceFunctionOutput {
                        type_f: match function_type {
                            FunctionType::Fixed(FixedFunction { returns, .. }) => {
                                ContractInterfaceAtomType::from_type_signature(returns)
                            }
                            _ => return Err(CheckErrors::Expects(
                                "Contract functions should only have fixed function return types!"
                                    .into(),
                            )
                            .into()),
                        },
                    },
                    args: match function_type {
                        FunctionType::Fixed(FixedFunction { args, .. }) => {
                            ContractInterfaceFunctionArg::from_function_args(args)
                        }
                        _ => {
                            return Err(CheckErrors::Expects(
                                "Contract functions should only have fixed function arguments!"
                                    .into(),
                            )
                            .into())
                        }
                    },
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContractInterfaceVariableAccess {
    constant,
    variable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub type_f: ContractInterfaceAtomType,
    pub access: ContractInterfaceVariableAccess,
}

impl ContractInterfaceFungibleTokens {
    pub fn from_set(tokens: &BTreeSet<ClarityName>) -> Vec<Self> {
        tokens
            .iter()
            .map(|name| Self {
                name: name.to_string(),
            })
            .collect()
    }
}

impl ContractInterfaceNonFungibleTokens {
    fn from_map(assets: &BTreeMap<ClarityName, TypeSignature>) -> Vec<Self> {
        assets
            .iter()
            .map(|(name, type_sig)| Self {
                name: name.clone().into(),
                type_f: ContractInterfaceAtomType::from_type_signature(type_sig),
            })
            .collect()
    }
}

impl ContractInterfaceVariable {
    fn from_map(
        map: &BTreeMap<ClarityName, TypeSignature>,
        access: ContractInterfaceVariableAccess,
    ) -> Vec<ContractInterfaceVariable> {
        map.iter()
            .map(|(name, type_sig)| ContractInterfaceVariable {
                name: name.clone().into(),
                access: access.to_owned(),
                type_f: ContractInterfaceAtomType::from_type_signature(type_sig),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterfaceMap {
    pub name: String,
    pub key: ContractInterfaceAtomType,
    pub value: ContractInterfaceAtomType,
}

impl ContractInterfaceMap {
    fn from_map(
        map: &BTreeMap<ClarityName, (TypeSignature, TypeSignature)>,
    ) -> Vec<ContractInterfaceMap> {
        map.iter()
            .map(|(name, (key_sig, val_sig))| ContractInterfaceMap {
                name: name.clone().into(),
                key: ContractInterfaceAtomType::from_type_signature(key_sig),
                value: ContractInterfaceAtomType::from_type_signature(val_sig),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractInterface {
    pub functions: Vec<ContractInterfaceFunction>,
    pub variables: Vec<ContractInterfaceVariable>,
    pub maps: Vec<ContractInterfaceMap>,
    pub fungible_tokens: Vec<ContractInterfaceFungibleTokens>,
    pub non_fungible_tokens: Vec<ContractInterfaceNonFungibleTokens>,
    pub epoch: StacksEpochId,
    pub clarity_version: ClarityVersion,
}

impl ContractInterface {
    pub fn new(epoch: StacksEpochId, clarity_version: ClarityVersion) -> Self {
        Self {
            functions: Vec::new(),
            variables: Vec::new(),
            maps: Vec::new(),
            fungible_tokens: Vec::new(),
            non_fungible_tokens: Vec::new(),
            epoch,
            clarity_version,
        }
    }

    pub fn serialize(&self) -> CheckResult<String> {
        serde_json::to_string(self).map_err(|_| {
            CheckErrors::Expects("Failed to serialize contract interface".into()).into()
        })
    }
}

#[test]
fn test_string_rename_ascii() {
    let arg = ContractInterfaceFunctionArg {
        name: "test-name".into(),
        type_f: ContractInterfaceAtomType::string_ascii { length: 32 },
    };
    assert_eq!(
        serde_json::to_string(&arg).unwrap(),
        "{\"name\":\"test-name\",\"type\":{\"string-ascii\":{\"length\":32}}}"
    );
}

#[test]
fn test_string_rename_utf8() {
    let arg = ContractInterfaceFunctionArg {
        name: "test-utf8".into(),
        type_f: ContractInterfaceAtomType::string_utf8 { length: 32 },
    };
    assert_eq!(
        serde_json::to_string(&arg).unwrap(),
        "{\"name\":\"test-utf8\",\"type\":{\"string-utf8\":{\"length\":32}}}"
    );
}
