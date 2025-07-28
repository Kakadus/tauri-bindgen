#![allow(
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::unused_self
)]

use heck::{ToKebabCase, ToLowerCamelCase, ToSnakeCase, ToUpperCamelCase};
use std::fmt::Write;
use std::path::PathBuf;
use tauri_bindgen_core::{postprocess, Generate, GeneratorBuilder, TypeInfo, TypeInfos};
use tauri_bindgen_gen_js::{JavaScriptGenerator, SerdeUtils};
use wit_parser::{
    EnumCase, FlagsField, Function, FunctionResult, Interface, RecordField, Type, TypeDefId,
    TypeDefKind, UnionCase, VariantCase,
};

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[cfg_attr(feature = "clap", clap(group(
    clap::ArgGroup::new("fmt")
        .args(&["prettier", "romefmt"]),
)))]
pub struct Builder {
    /// Run `prettier` to format the generated code. This requires a global installation of `prettier`.
    #[cfg_attr(feature = "clap", clap(long))]
    pub prettier: bool,
    /// Run `rome format` to format the generated code. This formatter is much faster than `prettier`. Requires a global installation of `rome`.
    #[cfg_attr(feature = "clap", clap(long))]
    pub romefmt: bool,
}

impl GeneratorBuilder for Builder {
    fn build(self, interface: Interface) -> Box<dyn Generate> {
        let methods = interface
            .typedefs
            .iter()
            .filter_map(|(_, typedef)| {
                if let TypeDefKind::Resource(methods) = &typedef.kind {
                    Some(methods.iter())
                } else {
                    None
                }
            })
            .flatten();

        let infos = TypeInfos::collect_from_functions(
            &interface.typedefs,
            interface.functions.iter().chain(methods),
        );

        let serde_utils =
            SerdeUtils::collect_from_functions(&interface.typedefs, &interface.functions);

        Box::new(TypeScript {
            opts: self,
            interface,
            infos,
            serde_utils,
        })
    }
}

#[derive(Debug)]
pub struct TypeScript {
    opts: Builder,
    interface: Interface,
    infos: TypeInfos,
    serde_utils: SerdeUtils,
}

impl TypeScript {
    pub fn print_function(&self, intf_name: &str, func: &Function) -> String {
        let docs = print_docs(&func.docs);

        let ident = func.id.to_lower_camel_case();
        let name = func.id.to_snake_case();

        let params = self.print_function_params(&func.params);

        let result = func
            .result
            .as_ref()
            .map_or("Promise<void>".to_string(), |result| {
                self.print_function_result(result)
            });

        let deserialize_result = func
            .result
            .as_ref()
            .map(|res| self.print_deserialize_function_result(res))
            .unwrap_or_default();

        let serialize_params = func
            .params
            .iter()
            .map(|(ident, ty)| self.print_serialize_ty(&ident.to_lower_camel_case(), ty))
            .collect::<Vec<_>>()
            .join(";\n");

        let (ret, as_ret) = if func.result.is_some() {
            ("return".to_string(), format!("as {result}"))
        } else {
            (String::new(), String::new())
        };

        format!(
            r#"
{docs}
export async function {ident} ({params}) : {result} {{
    const out = []
    {serialize_params}

    {ret} fetch('ipc://localhost/{intf_name}/{name}', {{ method: "POST", body: Uint8Array.from(out) }}){deserialize_result} {as_ret}
}}
        "#
        )
    }

    fn print_function_params(&self, params: &[(String, Type)]) -> String {
        params
            .iter()
            .map(|(ident, ty)| {
                let ident = ident.to_lower_camel_case();
                let ty = self.print_type(ty);

                format!("{ident}: {ty}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn print_function_result(&self, result: &FunctionResult) -> String {
        match result.len() {
            0 => "Promise<void>".to_string(),
            1 => {
                let ty = self.print_type(result.types().next().unwrap());
                format!("Promise<{ty}>")
            }
            _ => {
                let tys = result
                    .types()
                    .map(|ty| self.print_type(ty))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Promise<[{tys}]>")
            }
        }
    }

    fn print_type(&self, ty: &Type) -> String {
        match ty {
            Type::Bool => "boolean".to_string(),
            Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Float32
            | Type::Float64 => "number".to_string(),
            Type::U64 | Type::S64 | Type::U128 | Type::S128 => "bigint".to_string(),
            Type::Char | Type::String => "string".to_string(),
            Type::Tuple(types) => {
                let types = types
                    .iter()
                    .map(|ty| self.print_type(ty))
                    .collect::<Vec<_>>()
                    .join(", ");

                format!("[{types}]")
            }
            Type::List(ty) => {
                let ty = self.array_ty(ty).unwrap_or(self.print_type(ty));
                format!("{ty}[]")
            }
            Type::Option(ty) => {
                let ty = self.print_type(ty);

                format!("{ty} | null")
            }
            Type::Result { ok, err } => {
                let ok = ok
                    .as_ref()
                    .map_or("null".to_string(), |ty| self.print_type(ty));
                let err = err
                    .as_ref()
                    .map_or("null".to_string(), |ty| self.print_type(ty));

                format!("Result<{ok}, {err}>")
            }
            Type::Id(id) => self.interface.typedefs[*id].ident.to_upper_camel_case(),
        }
    }

    fn print_typedef(&self, id: TypeDefId) -> String {
        let typedef = &self.interface.typedefs[id];
        let ident = &typedef.ident.to_upper_camel_case();
        let docs = print_docs(&typedef.docs);

        match &typedef.kind {
            TypeDefKind::Alias(ty) => self.print_alias(&docs, ident, ty),
            TypeDefKind::Record(fields) => self.print_record(&docs, ident, fields),
            TypeDefKind::Flags(fields) => self.print_flags(&docs, ident, fields),
            TypeDefKind::Variant(cases) => self.print_variant(&docs, ident, cases),
            TypeDefKind::Enum(cases) => self.print_enum(&docs, ident, cases),
            TypeDefKind::Union(cases) => self.print_union(&docs, ident, cases),
            TypeDefKind::Resource(functions) => {
                self.print_resource(&self.interface.ident, &docs, ident, functions)
            }
        }
    }

    fn print_alias(&self, docs: &str, ident: &str, ty: &Type) -> String {
        let ty = self.print_type(ty);

        format!("{docs}\nexport type {ident} = {ty};\n")
    }

    fn print_record(&self, docs: &str, ident: &str, fields: &[RecordField]) -> String {
        let fields = fields.iter().fold(String::new(), |mut str, field| {
            let docs = print_docs(&field.docs);
            let ident = field.id.to_lower_camel_case();
            let ty = self.print_type(&field.ty);

            let _ = write!(str, "{docs}\n{ident}: {ty},\n");

            str
        });

        format!("{docs}\nexport interface {ident} {{ {fields} }}\n")
    }

    fn print_flags(&self, docs: &str, ident: &str, fields: &[FlagsField]) -> String {
        let fields = fields
            .iter()
            .enumerate()
            .fold(String::new(), |mut str, (i, field)| {
                let docs = print_docs(&field.docs);
                let ident = field.id.to_upper_camel_case();
                let value: u64 = 2 << i;

                let _ = write!(str, "{docs}\n{ident} = {value},\n");

                str
            });

        format!("{docs}\nexport enum {ident} {{ {fields} }}\n")
    }

    fn print_variant(&self, docs: &str, ident: &str, cases: &[VariantCase]) -> String {
        let interfaces: String =
            cases
                .iter()
                .enumerate()
                .fold(String::new(), |mut str, (i, case)| {
                    let docs = print_docs(&case.docs);
                    let case_ident = case.id.to_upper_camel_case();
                    let value = case
                        .ty
                        .as_ref()
                        .map(|ty| {
                            let ty = self.print_type(ty);
                            format!(", value: {ty}")
                        })
                        .unwrap_or_default();

                    let _ = write!(
                        str,
                        "{docs}\nexport interface {ident}{case_ident} {{ tag: {i}{value} }}\n"
                    );

                    str
                });

        let cases: String = cases
            .iter()
            .map(|case| {
                let docs = print_docs(&case.docs);
                let case_ident = case.id.to_upper_camel_case();

                format!("{docs}\n{ident}{case_ident}")
            })
            .collect::<Vec<_>>()
            .join(" | ");

        format!("{interfaces}\n{docs}\nexport type {ident} = {cases}\n")
    }

    fn print_enum(&self, docs: &str, ident: &str, cases: &[EnumCase]) -> String {
        let cases = cases.iter().fold(String::new(), |mut str, case| {
            let docs = print_docs(&case.docs);
            let ident = case.id.to_upper_camel_case();

            let _ = write!(str, "{docs}\n{ident},\n");

            str
        });

        format!("{docs}\nexport enum {ident} {{ {cases} }}\n")
    }

    fn print_union(&self, docs: &str, ident: &str, cases: &[UnionCase]) -> String {
        let cases: String = cases
            .iter()
            .map(|case| {
                let docs = print_docs(&case.docs);
                let ty = self.print_type(&case.ty);

                format!("{docs}\n{ty}\n")
            })
            .collect::<Vec<_>>()
            .join(" | ");

        format!("{docs}\nexport type {ident} = {cases};\n")
    }

    fn print_resource(
        &self,
        mod_ident: &str,
        docs: &str,
        ident: &str,
        functions: &[Function],
    ) -> String {
        let functions: String = functions
            .iter()
            .fold(String::new(),|mut str, func| {
                let docs = print_docs(&func.docs);

                let mod_ident = mod_ident.to_snake_case();
                let resource_ident = ident.to_snake_case();
                let ident = func.id.to_lower_camel_case();

                let params = self.print_function_params(&func.params);
                let result = func
                    .result
                    .as_ref()
                    .map_or("void".to_string(), |result| self.print_function_result(result));

                let deserialize_result = func
                    .result
                    .as_ref()
                    .map(|res| self.print_deserialize_function_result(res))
                    .unwrap_or_default();

                let serialize_params = func
                    .params
                    .iter()
                    .map(|(ident, ty)| self.print_serialize_ty(&ident.to_lower_camel_case(), ty))
                    .collect::<Vec<_>>()
                    .join(";\n");

                let _ = write!(str,
                    r#"{docs}
async {ident} ({params}) : {result} {{
    const out = []
    serializeU32(out, this.#id);
    {serialize_params}

    await fetch('ipc://localhost/{mod_ident}::resource::{resource_ident}/{ident}', {{ method: "POST", body: Uint8Array.from(out), headers: {{ 'Content-Type': 'application/octet-stream' }} }}){deserialize_result}
}}
"#
                );

                str
            });

        format!(
            "{docs}\nexport class {ident} {{
    #id: number;

    {functions}
}}"
        )
    }

    fn array_ty(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::U8 => Some("Uint8Array".to_string()),
            Type::S8 => Some("Int8Array".to_string()),
            Type::U16 => Some("Uint16Array".to_string()),
            Type::S16 => Some("Int16Array".to_string()),
            Type::U32 => Some("Uint32Array".to_string()),
            Type::S32 => Some("Int32Array".to_string()),
            Type::U64 => Some("BigUint64Array".to_string()),
            Type::S64 => Some("BigInt64Array".to_string()),
            Type::Float32 => Some("Float32Array".to_string()),
            Type::Float64 => Some("Float64Array".to_string()),
            Type::Id(id) => match &self.interface.typedefs[*id].kind {
                TypeDefKind::Alias(t) => self.array_ty(t),
                _ => None,
            },
            Type::U128
            | Type::S128
            | Type::Bool
            | Type::Tuple(_)
            | Type::List(_)
            | Type::Option(_)
            | Type::Result { .. }
            | Type::Char
            | Type::String => None,
        }
    }
}

fn print_docs(docs: &str) -> String {
    if docs.is_empty() {
        return String::new();
    }

    let docs = docs.lines().fold(String::new(), |mut str, line| {
        let _ = writeln!(str, " * {line}");
        str
    });

    format!("/**\n{docs}*/")
}

impl JavaScriptGenerator for TypeScript {
    fn interface(&self) -> &Interface {
        &self.interface
    }

    fn infos(&self) -> &TypeInfos {
        &self.infos
    }
}

impl Generate for TypeScript {
    fn to_file(&mut self) -> (PathBuf, String) {
        let ts_nocheck = "// @ts-nocheck\n".to_string();

        let result_ty = if self.interface.functions.iter().any(Function::throws) {
            "export type Result<T, E> = { tag: 'ok', val: T } | { tag: 'err', val: E };\n"
        } else {
            Default::default()
        };

        let serde_utils = self.serde_utils.to_string();

        let deserializers: String = self
            .interface
            .typedefs
            .iter()
            .filter_map(|(id, _)| {
                let info = self.infos[id];

                if info.contains(TypeInfo::RESULT) {
                    Some(self.print_deserialize_typedef(id))
                } else {
                    None
                }
            })
            .collect();

        let serializers: String = self
            .interface
            .typedefs
            .iter()
            .filter_map(|(id, _)| {
                let info = self.infos[id];

                if info.contains(TypeInfo::PARAM) {
                    Some(self.print_serialize_typedef(id))
                } else {
                    None
                }
            })
            .collect();

        let typedefs: String = self
            .interface
            .typedefs
            .iter()
            .map(|(id, _)| self.print_typedef(id))
            .collect();

        let functions: String = self
            .interface
            .functions
            .iter()
            .map(|func| self.print_function(&self.interface.ident.to_snake_case(), func))
            .collect();

        let mut contents = format!(
            "{ts_nocheck}{result_ty}{serde_utils}{deserializers}{serializers}\n{typedefs}\n{functions}"
        );

        if self.opts.prettier {
            postprocess(&mut contents, "prettier", ["--parser=typescript"])
                .expect("failed to run `rome format`");
        } else if self.opts.romefmt {
            postprocess(
                &mut contents,
                "rome",
                ["format", "--stdin-file-path", "index.ts"],
            )
            .expect("failed to run `rome format`");
        }

        let mut filename = PathBuf::from(self.interface.ident.to_kebab_case());
        filename.set_extension("ts");

        (filename, contents)
    }
}
