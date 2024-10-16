// Copyright 2024 Golem Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::call_type::CallType;
use crate::ParsedFunctionSite;
use golem_wasm_ast::analysis::AnalysedType;
use golem_wasm_ast::analysis::{AnalysedExport, TypeVariant};
use std::collections::{HashMap, HashSet};

// A type-registry is a mapping from a function name (global or part of an interface in WIT)
// to the registry value that represents the type of the name.
// Here, registry key names are called function names (and not really the names of the types),
// as this is what the component-model parser output (golem-wasm-ast) gives us.
// We make sure if we bump into any variant types (as part of processing the function parameter types),
// we store them as a mapping from FunctionName(name_of_variant) to a registry value. If the variant
// has parameters, then the RegistryValue is considered a function type itself with parameter types,
// and a return type that the member variant represents. If the variant has no parameters,
// then the RegistryValue is simply an AnalysedType representing the variant type itself.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum RegistryKey {
    FunctionName(String),
    FunctionNameWithInterface {
        interface_name: String,
        function_name: String,
    },
}

impl RegistryKey {
    pub fn from_function_name(site: &ParsedFunctionSite, function_name: &str) -> RegistryKey {
        match site.interface_name() {
            None => RegistryKey::FunctionName(function_name.to_string()),
            Some(name) => RegistryKey::FunctionNameWithInterface {
                interface_name: name.to_string(),
                function_name: function_name.to_string(),
            },
        }
    }
    pub fn from_call_type(call_type: &CallType) -> RegistryKey {
        match call_type {
            CallType::VariantConstructor(variant_name) => {
                RegistryKey::FunctionName(variant_name.clone())
            }
            CallType::EnumConstructor(enum_name) => RegistryKey::FunctionName(enum_name.clone()),
            CallType::Function(function_name) => match function_name.site.interface_name() {
                None => RegistryKey::FunctionName(function_name.function_name()),
                Some(interface_name) => RegistryKey::FunctionNameWithInterface {
                    interface_name: interface_name.to_string(),
                    function_name: function_name.function_name(),
                },
            },
        }
    }
}

#[derive(PartialEq, Clone, Debug)]
pub enum RegistryValue {
    Value(AnalysedType),
    Variant {
        parameter_types: Vec<AnalysedType>,
        variant_type: TypeVariant,
    },
    Function {
        parameter_types: Vec<AnalysedType>,
        return_types: Vec<AnalysedType>,
    },
}

impl RegistryValue {
    pub fn argument_types(&self) -> Vec<AnalysedType> {
        match self {
            RegistryValue::Function {
                parameter_types,
                return_types: _,
            } => parameter_types.clone(),
            RegistryValue::Variant {
                parameter_types,
                variant_type: _,
            } => parameter_types.clone(),
            RegistryValue::Value(_) => vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FunctionTypeRegistry {
    pub types: HashMap<RegistryKey, RegistryValue>,
}

impl FunctionTypeRegistry {
    pub fn get_variants(&self) -> Vec<TypeVariant> {
        let mut variants = vec![];

        for registry_value in self.types.values() {
            if let RegistryValue::Variant { variant_type, .. } = registry_value {
                variants.push(variant_type.clone())
            }
        }

        variants
    }

    pub fn get(&self, key: &CallType) -> Option<&RegistryValue> {
        match key {
            CallType::Function(parsed_fn_name) => self.types.get(&RegistryKey::from_function_name(
                &parsed_fn_name.site,
                &parsed_fn_name.function_name(),
            )),
            CallType::VariantConstructor(variant_name) => self
                .types
                .get(&RegistryKey::FunctionName(variant_name.clone())),
            CallType::EnumConstructor(enum_name) => self
                .types
                .get(&RegistryKey::FunctionName(enum_name.clone())),
        }
    }

    pub fn empty() -> Self {
        Self {
            types: HashMap::new(),
        }
    }

    pub fn from_export_metadata(exports: &Vec<AnalysedExport>) -> Self {
        let mut map = HashMap::new();

        let mut types = HashSet::new();

        for export in exports {
            match export {
                AnalysedExport::Instance(ty) => {
                    let interface_name = &ty.name;
                    for fun in ty.functions.clone() {
                        let function_name = fun.name;
                        let parameter_types = fun
                            .parameters
                            .into_iter()
                            .map(|parameter| {
                                let analysed_type = parameter.typ;
                                types.insert(analysed_type.clone());
                                analysed_type
                            })
                            .collect::<Vec<_>>();

                        let return_types = fun
                            .results
                            .into_iter()
                            .map(|result| {
                                let analysed_type = result.typ;
                                types.insert(analysed_type.clone());
                                analysed_type
                            })
                            .collect::<Vec<_>>();

                        let registry_key = RegistryKey::FunctionNameWithInterface {
                            interface_name: interface_name.clone(),
                            function_name: function_name.clone(),
                        };

                        let registry_value = RegistryValue::Function {
                            parameter_types,
                            return_types,
                        };

                        map.insert(registry_key, registry_value);
                    }
                }
                AnalysedExport::Function(fun0) => {
                    let fun = fun0.clone();
                    let function_name = fun.name;
                    let parameter_types = fun
                        .parameters
                        .into_iter()
                        .map(|parameter| {
                            let analysed_type = parameter.typ;
                            types.insert(analysed_type.clone());
                            analysed_type
                        })
                        .collect::<Vec<_>>();

                    let return_types = fun
                        .results
                        .into_iter()
                        .map(|result| {
                            let analysed_type = result.typ;
                            types.insert(analysed_type.clone());
                            analysed_type
                        })
                        .collect::<Vec<_>>();

                    let registry_value = RegistryValue::Function {
                        parameter_types,
                        return_types,
                    };

                    let registry_key = RegistryKey::FunctionName(function_name.clone());

                    map.insert(registry_key, registry_value);
                }
            }
        }

        for ty in types {
            internal::update_registry(&ty, &mut map);
        }

        Self { types: map }
    }

    pub fn lookup(&self, registry_key: &RegistryKey) -> Option<RegistryValue> {
        self.types.get(registry_key).cloned()
    }
}

mod internal {
    use crate::{RegistryKey, RegistryValue};
    use golem_wasm_ast::analysis::{AnalysedType, TypeResult};
    use std::collections::HashMap;

    pub(crate) fn update_registry(
        ty: &AnalysedType,
        registry: &mut HashMap<RegistryKey, RegistryValue>,
    ) {
        match ty.clone() {
            AnalysedType::Variant(variant) => {
                let type_variant = variant.clone();
                for name_type_pair in &type_variant.cases {
                    registry.insert(RegistryKey::FunctionName(name_type_pair.name.clone()), {
                        name_type_pair.typ.clone().map_or(
                            RegistryValue::Value(ty.clone()),
                            |variant_parameter_typ| RegistryValue::Variant {
                                parameter_types: vec![variant_parameter_typ],
                                variant_type: type_variant.clone(),
                            },
                        )
                    });
                }
            }

            AnalysedType::Enum(type_enum) => {
                for name_type_pair in type_enum.cases {
                    registry.insert(
                        RegistryKey::FunctionName(name_type_pair.clone()),
                        RegistryValue::Value(ty.clone()),
                    );
                }
            }

            AnalysedType::Tuple(tuple) => {
                for element in tuple.items {
                    update_registry(&element, registry);
                }
            }

            AnalysedType::List(list) => {
                update_registry(list.inner.as_ref(), registry);
            }

            AnalysedType::Record(record) => {
                for name_type in record.fields.iter() {
                    update_registry(&name_type.typ, registry);
                }
            }

            AnalysedType::Result(TypeResult {
                ok: Some(ok_type),
                err: Some(err_type),
            }) => {
                update_registry(ok_type.as_ref(), registry);
                update_registry(err_type.as_ref(), registry);
            }
            AnalysedType::Result(TypeResult {
                ok: None,
                err: Some(err_type),
            }) => {
                update_registry(err_type.as_ref(), registry);
            }
            AnalysedType::Result(TypeResult {
                ok: Some(ok_type),
                err: None,
            }) => {
                update_registry(ok_type.as_ref(), registry);
            }
            AnalysedType::Option(type_option) => {
                update_registry(type_option.inner.as_ref(), registry);
            }
            AnalysedType::Result(TypeResult {
                ok: None,
                err: None,
            }) => {}
            AnalysedType::Flags(_) => {}
            AnalysedType::Str(_) => {}
            AnalysedType::Chr(_) => {}
            AnalysedType::F64(_) => {}
            AnalysedType::F32(_) => {}
            AnalysedType::U64(_) => {}
            AnalysedType::S64(_) => {}
            AnalysedType::U32(_) => {}
            AnalysedType::S32(_) => {}
            AnalysedType::U16(_) => {}
            AnalysedType::S16(_) => {}
            AnalysedType::U8(_) => {}
            AnalysedType::S8(_) => {}
            AnalysedType::Bool(_) => {}
            AnalysedType::Handle(_) => {}
        }
    }
}
