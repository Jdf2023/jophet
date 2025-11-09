// src/backend/c/declarations.rs
//! Handles the generation of forward declarations for the C target representation.
//!
//! This part of the C backend performs a first pass over the typed AST and all
//! imported module scopes. It scans for all definitions like structs, enums, unions,
//! and functions, and generates the corresponding C `typedef`s and function prototypes.
//! This ensures that all types and functions are declared before they are used in the
//! generated C code, resolving potential order-of-definition issues. It correctly
//! handles types defined locally as well as those brought into scope via any form
//! of `import` statement.

use super::Generator;
use crate::core::ast::typed::*;
use crate::core::semantic_analyzer::ModuleScope;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::PathBuf;

impl Generator {
    /// Performs a multi-stage pass over the program to generate C type and function definitions.
    ///
    /// This method operates in multiple stages:
    /// 1. Pre-scans to identify all structs, tagged unions, and errors that will require
    ///    a destructor or clone function based on whether they contain owned data. This now
    ///    scans both the current module's AST and all imported modules.
    /// 2. Caches all user-defined `error` type names (from all modules) to correctly generate
    ///    the universal `JophetError` struct if needed.
    /// 3. Caches all struct, enum, and other type definitions from all modules for use in later stages.
    /// 4. The main pass iterates through the AST of the *current module* and all *imported modules*,
    ///    generating definitions and prototypes for all types (including errors) and functions.
    ///    This prevents redefinition errors and ensures all necessary types are defined.
    pub fn forward_declare(
        &mut self,
        program: &TypedProgram,
        imported_modules: &HashMap<String, ModuleScope>,
        all_error_defs: &[TypedErrorDef],
    ) {
        // --- STAGE 1: PRE-SCANS FOR DESTRUCTORS AND CLONERS ---

        // A. Pre-scan the current module's AST
        for stmt in program {
            if let TypedStatementKind::StructDef(def) = &stmt.kind {
                if def.fields.iter().any(|(_, field_type, _)| self.type_needs_cleanup(field_type)) {
                    self.structs_with_destructors.insert(def.name.clone());
                }
                if def.fields.iter().all(|(_, field_type, _)| self.type_is_cloneable(field_type)) {
                    self.cloneable_structs.insert(def.name.clone());
                }
            }
            if let TypedStatementKind::TaggedUnionDef(def) = &stmt.kind {
                if def.variants.iter().any(|v| v.payload.as_ref().map_or(false, |p| self.type_needs_cleanup(p))) {
                    self.tagged_unions_with_destructors.insert(def.name.clone());
                }
                if def.variants.iter().all(|v| v.payload.as_ref().map_or(true, |p| self.type_is_cloneable(p))) {
                    self.cloneable_tagged_unions.insert(def.name.clone());
                }
            }
            if let TypedStatementKind::ErrorDef(def) = &stmt.kind {
                 if def.variants.iter().any(|v| v.payload.as_ref().map_or(false, |p| self.type_needs_cleanup(p))) {
                    self.tagged_unions_with_destructors.insert(def.name.clone());
                 }
                 if def.variants.iter().all(|v| v.payload.as_ref().map_or(true, |p| self.type_is_cloneable(p))) {
                     self.cloneable_tagged_unions.insert(def.name.clone());
                 }
            }
        }
        
        // B. Pre-scan imported modules. Clone to avoid borrow checker issues.
        let imported_modules_clone: Vec<_> = imported_modules.values().cloned().collect();
        for module_scope in &imported_modules_clone {
            for def in module_scope.struct_defs.values() {
                if def.fields.iter().any(|(_, field_type, _)| self.type_needs_cleanup(field_type)) {
                    self.structs_with_destructors.insert(def.name.clone());
                }
                if def.fields.iter().all(|(_, field_type, _)| self.type_is_cloneable(field_type)) {
                    self.cloneable_structs.insert(def.name.clone());
                }
            }
            for def in module_scope.tagged_union_defs.values() {
                if def.variants.iter().any(|v| v.payload.as_ref().map_or(false, |p| self.type_needs_cleanup(p))) {
                    self.tagged_unions_with_destructors.insert(def.name.clone());
                }
                if def.variants.iter().all(|v| v.payload.as_ref().map_or(true, |p| self.type_is_cloneable(p))) {
                    self.cloneable_tagged_unions.insert(def.name.clone());
                }
            }
            for def in module_scope.error_defs.values() {
                 if def.variants.iter().any(|v| v.payload.as_ref().map_or(false, |p| self.type_needs_cleanup(p))) {
                    self.tagged_unions_with_destructors.insert(def.name.clone());
                 }
                 if def.variants.iter().all(|v| v.payload.as_ref().map_or(true, |p| self.type_is_cloneable(p))) {
                     self.cloneable_tagged_unions.insert(def.name.clone());
                 }
            }
        }

        // --- STAGE 2: CACHING AND PREPARATION ---

        // Use the complete list of error definitions passed from the analyzer.
        self.all_error_types.clear();
        for def in all_error_defs {
            self.all_error_types.insert(def.name.clone());
        }

        // Cache all available type definitions for later use.
        for stmt in program { // 1. From current program
            if let TypedStatementKind::StructDef(def) = &stmt.kind {
                self.struct_defs_cache.insert(def.name.clone(), def.clone());
            }
        }
        for module_scope in imported_modules.values() { // 2. From imported modules
            for (name, def) in &module_scope.struct_defs {
                self.struct_defs_cache.insert(name.clone(), def.clone());
            }
            for (name, def) in &module_scope.enum_defs {
                self.enum_defs_cache.insert(name.clone(), def.clone());
            }
            for (name, def) in &module_scope.tagged_union_defs {
                self.tagged_union_defs_cache.insert(name.clone(), def.clone());
            }
            for (name, def) in &module_scope.error_defs {
                self.error_defs_cache.insert(name.clone(), def.clone());
            }
        }

        // --- STAGE 3: MAIN DEFINITION GENERATION ---

        // A. Generate for the current module's AST.
        for stmt in program {
            match &stmt.kind {
                TypedStatementKind::StructDef(def) => self.compile_struct_def(def, true),
                TypedStatementKind::EnumDef(def) => {
                    self.enum_defs_cache.insert(def.name.clone(), def.clone());
                    self.compile_enum_def(def);
                }
                TypedStatementKind::UnionDef(def) => self.compile_union_def(def, true),
                TypedStatementKind::TaggedUnionDef(def) => {
                    self.tagged_union_defs_cache.insert(def.name.clone(), def.clone());
                    self.compile_tagged_union_def(def, true);
                }
                TypedStatementKind::ErrorDef(def) => {
                    self.error_defs_cache.insert(def.name.clone(), def.clone());
                    self.compile_error_def(def);
                }
                TypedStatementKind::FunctionDecl(decl) => self.compile_function_declaration(decl),
                _ => {}
            }
        }

        // B. Generate for imported modules' public types.
        for module_scope in imported_modules.values() {
            for def in module_scope.struct_defs.values() {
                if def.is_public { self.compile_struct_def(def, false); }
            }
            for def in module_scope.enum_defs.values() {
                if def.is_public { self.compile_enum_def(def); }
            }
            for def in module_scope.union_defs.values() {
                if def.is_public { self.compile_union_def(def, false); }
            }
            for def in module_scope.tagged_union_defs.values() {
                if def.is_public { self.compile_tagged_union_def(def, false); }
            }
            for def in module_scope.error_defs.values() {
                if def.is_public { self.compile_error_def(def); }
            }
        }
    }

    /// Generates the suite of helper functions required to print a specific `Dictionary<K, V>` type.
    pub fn generate_dictionary_print_function(&mut self, key_type: &JophetType, value_type: &JophetType) {
        let key_c_type = self.jophet_type_to_c_string(key_type);
        let value_c_type = self.jophet_type_to_c_string(value_type);

        let key_mangled = self.jophet_type_to_c_string_for_mangling(key_type);
        let value_mangled = self.jophet_type_to_c_string_for_mangling(value_type);

        let main_print_fn_name = format!("__jophet_print_dictionary_of_{}_{}", key_mangled, value_mangled);
        
        // Avoid re-generating if we've already created this helper.
        let proto_to_check = format!("void {}(const JophetDictionary* d);", main_print_fn_name);
        if self.function_prototypes.contains(&proto_to_check) {
            return;
        }

        // 1. Generate the key print thunk.
        let key_thunk_name = format!("__jophet_print_key_thunk_{}", key_mangled);
        let key_print_expr = format!("((const {}*)data)", key_c_type);
        let key_print_call = self.get_print_call(key_type, &key_print_expr, true);
        let key_thunk_def = format!(
            "static void {}(const void* data) {{\n\t{};\n}}",
            key_thunk_name, key_print_call
        );
        writeln!(&mut self.function_defs, "{}\n", key_thunk_def).unwrap();

        // 2. Generate the value print thunk.
        let value_thunk_name = format!("__jophet_print_value_thunk_{}", value_mangled);
        let value_print_expr = format!("((const {}*)data)", value_c_type);
        let value_print_call = self.get_print_call(value_type, &value_print_expr, true);
        let value_thunk_def = format!(
            "static void {}(const void* data) {{\n\t{};\n}}",
            value_thunk_name, value_print_call
        );
        writeln!(&mut self.function_defs, "{}\n", value_thunk_def).unwrap();
        
        // 3. Generate the main dictionary print function.
        self.function_prototypes.insert(proto_to_check);
        let main_print_def = format!(
            "void {}(const JophetDictionary* d) {{\n\tDictionary_print(d, &{}, &{});\n}}",
            main_print_fn_name, key_thunk_name, value_thunk_name
        );
        writeln!(&mut self.function_defs, "{}\n", main_print_def).unwrap();
    }

    /// Compiles a Jophet `struct` definition into its C representation, a `typedef struct`.
    ///
    /// This function also generates several helper functions for the struct:
    /// 1. A `StructName_new` constructor function that heap-allocates and initializes the struct.
    /// 2. A `StructName_print` function for debugging.
    /// 3. If the struct is cloneable, a `StructName_clone` function for deep copying.
    /// 4. If the struct has owned fields, a `StructName_delete` function for cleanup.
    ///
    /// If `generate_bodies` is false, only the `typedef` and function prototypes are generated,
    /// which is crucial for handling types imported from other modules to avoid linker errors.
    fn compile_struct_def(&mut self, def: &TypedStructDef, generate_bodies: bool) {
        // To prevent redefinition errors, check if the print function's prototype already exists.
        // This is a reliable indicator that the type has already been fully generated in this file.
        let print_fn_name = format!("{}_print", def.name);
        let print_proto = format!("void {}(const {}* s);", print_fn_name, def.name);
        if self.function_prototypes.contains(&print_proto) {
            return;
        }

        let mut c_fields = Vec::new();
        let mut destructor_body = String::new();
        let mut print_body = String::new();
        let mut constructor_params = Vec::new();
        let mut constructor_body = String::new();
        let mut clone_body = String::new();

        if generate_bodies {
            writeln!(
                &mut constructor_body,
                "\t{name}* self = ({name}*)calloc(1, sizeof({name}));",
                name = def.name
            )
            .unwrap();

            if self.cloneable_structs.contains(&def.name) {
                writeln!(
                    &mut clone_body,
                    "\t{name}* new_s = ({name}*)calloc(1, sizeof({name}));",
                    name = def.name
                )
                .unwrap();
            }

            // Generate the print function body
            writeln!(&mut print_body, "\tprintf(\"{} {{ \");", def.name).unwrap();
        }

        for (i, (name, jophet_type, _is_public)) in def.fields.iter().enumerate() {
            let c_type = self.jophet_type_to_c_string(jophet_type);
            let sanitized_name = self.sanitize_c_keyword(name);
            c_fields.push(format!("{} {};", c_type, sanitized_name));

            if generate_bodies {
                // Build constructor
                constructor_params.push(format!("{} {}", c_type, sanitized_name));
                writeln!(&mut constructor_body, "\tself->{} = {};", sanitized_name, sanitized_name).unwrap();

                // Build clone function body
                if self.cloneable_structs.contains(&def.name) {
                    let clone_call = self.get_clone_call(jophet_type, &format!("s->{}", sanitized_name));
                    writeln!(&mut clone_body, "\tnew_s->{} = {};", sanitized_name, clone_call).unwrap();
                }

                // Check if this field needs cleanup and build the destructor body.
                let cleanup_call = self.get_cleanup_call(jophet_type, &format!("s->{}", sanitized_name), false);
                if !cleanup_call.is_empty() {
                    writeln!(&mut destructor_body, "\t{};", cleanup_call).unwrap();
                }

                // Build the print function body
                writeln!(&mut print_body, "\tprintf(\"{}: \");", sanitized_name).unwrap();
                let print_call = self.get_print_call(jophet_type, &format!("s->{}", sanitized_name), false);
                writeln!(&mut print_body, "\t{};", print_call).unwrap();
                if i < def.fields.len() - 1 {
                    writeln!(&mut print_body, "\tprintf(\", \");").unwrap();
                }
            }
        }
        
        if generate_bodies {
            writeln!(&mut constructor_body, "\treturn self;").unwrap();
            writeln!(&mut print_body, "\tprintf(\" }}\");").unwrap();

            if self.cloneable_structs.contains(&def.name) {
                writeln!(&mut clone_body, "\treturn new_s;").unwrap();
            }
        }

        let doc_comment = self.format_doc_comment(&def.doc_comment);
        let c_struct = format!(
            "{}\ntypedef struct {} {{ {} }} {};",
            doc_comment,
            def.name,
            c_fields.join(" "),
            def.name
        );
        self.type_defs.insert(c_struct.trim_start().to_string());

        // Generate the constructor function prototype
        let constructor_name = format!("{}_new", def.name);
        let constructor_proto = format!(
            "{}* {}({});",
            def.name,
            constructor_name,
            def.fields.iter().map(|(name, ty, _)| format!("{} {}", self.jophet_type_to_c_string(ty), self.sanitize_c_keyword(name))).collect::<Vec<_>>().join(", ")
        );
        self.function_prototypes.insert(constructor_proto);
        
        // Generate the print function prototype
        self.function_prototypes.insert(print_proto);

        // If the struct is cloneable, define its clone function prototype.
        if self.cloneable_structs.contains(&def.name) {
            let clone_name = format!("{}_clone", def.name);
            let clone_proto = format!("{}* {}(const {}* s);", def.name, clone_name, def.name);
            self.function_prototypes.insert(clone_proto);
        }

        // If the struct needs a destructor, define its prototype.
        if self.structs_with_destructors.contains(&def.name) {
            let destructor_name = format!("{}_delete", def.name);
            let destructor_proto = format!("void {}({}* s);", destructor_name, def.name);
            self.function_prototypes.insert(destructor_proto);
        }

        // Only generate the full function bodies if this struct is defined in the current module.
        if generate_bodies {
            let constructor_def_params = def.fields.iter().map(|(name, ty, _)| format!("{} {}", self.jophet_type_to_c_string(ty), self.sanitize_c_keyword(name))).collect::<Vec<_>>().join(", ");
            let constructor_def = format!(
                "{}* {}({}) {{\n{}}}",
                def.name,
                constructor_name,
                constructor_def_params,
                constructor_body
            );
            writeln!(&mut self.function_defs, "{}\n", constructor_def).unwrap();
            
            let print_def = format!(
                "void {}(const {}* s) {{\n{}}}",
                print_fn_name, def.name, print_body
            );
            writeln!(&mut self.function_defs, "{}\n", print_def).unwrap();

            if self.cloneable_structs.contains(&def.name) {
                let clone_name = format!("{}_clone", def.name);
                let clone_def = format!(
                    "{}* {}(const {}* s) {{\n{}}}",
                    def.name, clone_name, def.name, clone_body
                );
                writeln!(&mut self.function_defs, "{}\n", clone_def).unwrap();
            }

            if self.structs_with_destructors.contains(&def.name) {
                let destructor_name = format!("{}_delete", def.name);
                let destructor_def = format!(
                    "void {}({}* s) {{\n{}}}",
                    destructor_name, def.name, destructor_body
                );
                writeln!(&mut self.function_defs, "{}\n", destructor_def).unwrap();
            }
        }
    }

    /// Compiles a Jophet `enum` definition into its C representation, a `typedef enum`.
    ///
    /// The generated C enum has the same name as the Jophet enum. Its members are
    /// prefixed with the enum's name and assigned their explicit integer values.
    ///
    /// Example: `typedef enum MyEnum { MyEnum_A = 1, MyEnum_B = 5 } MyEnum;`
    fn compile_enum_def(&mut self, def: &TypedEnumDef) {
        let prefixed_members: Vec<String> = def
            .members
            .iter()
            .map(|(name, value, _doc)| format!("{}_{} = {}", def.name, name, value))
            .collect();
        let doc_comment = self.format_doc_comment(&def.doc_comment);
        let c_enum = format!(
            "{}\ntypedef enum {} {{ {} }} {};",
            doc_comment,
            def.name,
            prefixed_members.join(", "),
            def.name
        );
        // Using insert on IndexSet handles deduplication automatically.
        self.type_defs.insert(c_enum.trim_start().to_string());
    }

    /// Compiles a Jophet `union` definition into its C representation, a `typedef union`.
    /// This also generates a `UnionName_print` helper function for debugging.
    ///
    /// If `generate_bodies` is false, only the `typedef` and print prototype are generated.
    fn compile_union_def(&mut self, def: &TypedUnionDef, generate_bodies: bool) {
        // Deduplication check
        let print_fn_name = format!("{}_print", def.name);
        let print_proto = format!("void {}(const {}* u);", print_fn_name, def.name);
        if self.function_prototypes.contains(&print_proto) {
            return;
        }

        let mut c_fields = Vec::new();
        let mut print_body = String::new();

        if generate_bodies {
            writeln!(&mut print_body, "\tprintf(\"{} {{ \");", def.name).unwrap();
        }

        for (i, (name, jophet_type, _doc_comment)) in def.fields.iter().enumerate() {
            let sanitized_name = self.sanitize_c_keyword(name);
            c_fields.push(format!(
                "{} {};",
                self.jophet_type_to_c_string(jophet_type),
                sanitized_name
            ));
            
            if generate_bodies {
                // Build the print function body to show all possible interpretations
                writeln!(&mut print_body, "\tprintf(\"{}: \");", sanitized_name).unwrap();
                let print_call = self.get_print_call(jophet_type, &format!("u->{}", sanitized_name), false);
                writeln!(&mut print_body, "\t{};", print_call).unwrap();
                if i < def.fields.len() - 1 {
                    writeln!(&mut print_body, "\tprintf(\", \");").unwrap();
                }
            }
        }
        
        if generate_bodies {
            writeln!(&mut print_body, "\tprintf(\" }}\");").unwrap();
        }

        let doc_comment = self.format_doc_comment(&def.doc_comment);
        let c_union = format!(
            "{}\ntypedef union {} {{ {} }} {};",
            doc_comment,
            def.name,
            c_fields.join(" "),
            def.name
        );
        self.type_defs.insert(c_union.trim_start().to_string());
        
        // Generate the print function prototype
        self.function_prototypes.insert(print_proto);

        if generate_bodies {
            let print_def = format!(
                "void {}(const {}* u) {{\n{}}}",
                print_fn_name, def.name, print_body
            );
            writeln!(&mut self.function_defs, "{}\n", print_def).unwrap();
        }
    }

    /// A helper function to compile Jophet tagged unions and error types into C structures.
    ///
    /// If `generate_bodies` is false, only the `typedef`s and function prototypes are generated.
    fn compile_tagged_like_def(
        &mut self,
        name: &str,
        variants: &[TypedTaggedUnionVariant],
        doc_comment: &Option<String>,
        generate_bodies: bool,
    ) {
        // To prevent redefinition errors, check if the print function's prototype already exists.
        // This is a reliable indicator that the type has already been fully generated in this file.
        let print_fn_name = format!("{}_print", name);
        let print_proto = format!("void {}(const {}* s);", print_fn_name, name);
        if self.function_prototypes.contains(&print_proto) {
            return;
        }

        // 1. Create the tag enum.
        let tag_enum_name = format!("{}_Tag", name);
        let variant_names: Vec<String> = variants
            .iter()
            .map(|v| format!("{}_{}", name, v.name))
            .collect();
        let tag_enum = format!(
            "typedef enum {{ {} }} {};",
            variant_names.join(", "),
            tag_enum_name
        );
        self.type_defs.insert(tag_enum);

        // 2. Create the data union.
        let mut union_fields = Vec::new();
        for variant in variants {
            if let Some(payload_type) = &variant.payload {
                union_fields.push(format!(
                    "{} {};",
                    self.jophet_type_to_c_string(payload_type),
                    variant.name
                ));
            }
        }
        let data_union_name = format!("{}_Data", name);
        let union_body = if union_fields.is_empty() {
            "uint8_t _dummy;".to_string()
        } else {
            union_fields.join(" ")
        };
        let data_union = format!(
            "typedef union {{ {} }} {};",
            union_body, data_union_name
        );
        self.type_defs.insert(data_union);

        // 3. Create the final struct.
        let c_doc_comment = self.format_doc_comment(doc_comment);
        let tagged_struct = format!(
            "{}\ntypedef struct {} {{ {} tag; {} data; }} {};",
            c_doc_comment, name, tag_enum_name, data_union_name, name
        );
        self.type_defs.insert(tagged_struct.trim_start().to_string());

        // 4. Generate prototypes for helper functions.
        self.function_prototypes.insert(print_proto);
        if self.tagged_unions_with_destructors.contains(name) {
            let delete_fn_name = format!("{}_delete", name);
            let delete_proto = format!("void {}({}* s);", delete_fn_name, name);
            self.function_prototypes.insert(delete_proto);
        }
        if self.cloneable_tagged_unions.contains(name) {
            let clone_fn_name = format!("{}_clone", name);
            let clone_proto = format!("{} {}(const {}* s);", name, clone_fn_name, name);
            self.function_prototypes.insert(clone_proto);
        }
        
        // 5. Generate function bodies only if needed.
        if generate_bodies {
            let mut print_body = String::new();
            writeln!(&mut print_body, "\tswitch (s->tag) {{").unwrap();
            for variant in variants {
                let full_variant_name = format!("{}_{}", name, variant.name);
                writeln!(&mut print_body, "\t\tcase {}: {{", full_variant_name).unwrap();
                if let Some(payload_type) = &variant.payload {
                    writeln!(&mut print_body, "\t\t\tprintf(\"{}.{}(\");", name, variant.name).unwrap();
                    let print_call = self.get_print_call(payload_type, &format!("s->data.{}", variant.name), false);
                    writeln!(&mut print_body, "\t\t\t{};", print_call).unwrap();
                    writeln!(&mut print_body, "\t\t\tprintf(\")\");").unwrap();
                } else {
                    writeln!(&mut print_body, "\t\t\tprintf(\"{}.{}\");", name, variant.name).unwrap();
                }
                writeln!(&mut print_body, "\t\t\tbreak;").unwrap();
                writeln!(&mut print_body, "\t\t}}").unwrap();
            }
            writeln!(&mut print_body, "\t}}").unwrap();
            let print_def = format!("void {}(const {}* s) {{\n{}}}", print_fn_name, name, print_body);
            writeln!(&mut self.function_defs, "{}\n", print_def).unwrap();

            if self.tagged_unions_with_destructors.contains(name) {
                let delete_fn_name = format!("{}_delete", name);
                let mut delete_body = String::new();
                writeln!(&mut delete_body, "\tswitch (s->tag) {{").unwrap();
                 for variant in variants {
                     if let Some(payload_type) = &variant.payload {
                         let cleanup_call = self.get_cleanup_call(payload_type, &format!("s->data.{}", variant.name), false);
                         if !cleanup_call.is_empty() {
                             let full_variant_name = format!("{}_{}", name, variant.name);
                             writeln!(&mut delete_body, "\t\tcase {}: {{", full_variant_name).unwrap();
                             writeln!(&mut delete_body, "\t\t\t{};", cleanup_call).unwrap();
                             writeln!(&mut delete_body, "\t\t\tbreak;").unwrap();
                             writeln!(&mut delete_body, "\t\t}}").unwrap();
                         }
                     }
                }
                writeln!(&mut delete_body, "\t\tdefault: break; // No cleanup needed for other variants").unwrap();
                writeln!(&mut delete_body, "\t}}").unwrap();
                let delete_def = format!("void {}({}* s) {{\n{}}}", delete_fn_name, name, delete_body);
                writeln!(&mut self.function_defs, "{}\n", delete_def).unwrap();
            }

            if self.cloneable_tagged_unions.contains(name) {
                let clone_fn_name = format!("{}_clone", name);
                let mut clone_body = String::new();
                writeln!(&mut clone_body, "\t{} new_obj;", name).unwrap();
                writeln!(&mut clone_body, "\tnew_obj.tag = s->tag;").unwrap();
                writeln!(&mut clone_body, "\tswitch (s->tag) {{").unwrap();
                for variant in variants {
                    if let Some(payload_type) = &variant.payload {
                        let clone_call = self.get_clone_call(payload_type, &format!("s->data.{}", variant.name));
                         let full_variant_name = format!("{}_{}", name, variant.name);
                         writeln!(&mut clone_body, "\t\tcase {}: {{", full_variant_name).unwrap();
                         writeln!(&mut clone_body, "\t\t\tnew_obj.data.{} = {};", variant.name, clone_call).unwrap();
                         writeln!(&mut clone_body, "\t\t\tbreak;").unwrap();
                         writeln!(&mut clone_body, "\t\t}}").unwrap();
                    }
                }
                writeln!(&mut clone_body, "\t\tdefault: break; // No payload to clone").unwrap();
                writeln!(&mut clone_body, "\t}}").unwrap();
                writeln!(&mut clone_body, "\treturn new_obj;").unwrap();
                let clone_def = format!("{} {}(const {}* s) {{\n{}}}", name, clone_fn_name, name, clone_body);
                writeln!(&mut self.function_defs, "{}\n", clone_def).unwrap();
            }
        }
    }

    /// Compiles a Jophet `tagged union` definition by delegating to `compile_tagged_like_def`.
    fn compile_tagged_union_def(&mut self, def: &TypedTaggedUnionDef, generate_bodies: bool) {
        self.compile_tagged_like_def(&def.name, &def.variants, &def.doc_comment, generate_bodies);
    }

    /// Compiles a Jophet `error` definition by delegating to `compile_tagged_like_def`.
    /// This reuses the same C structure as a tagged union for the implementation of error types.
    fn compile_error_def(&mut self, def: &TypedErrorDef) {
        // Error definitions are always fully generated as they are part of the program's error handling logic.
        self.compile_tagged_like_def(&def.name, &def.variants, &def.doc_comment, true);
    }

    /// Checks if a method's signature indicates it mutates `self`.
    pub(super) fn is_method_mutating(&self, decl: &TypedFunctionDecl) -> bool {
        // A method is mutating if its `self` parameter is a `MutableReference`.
        // This is a simple but effective heuristic for now.
        if let Some((name, self_type)) = decl.params.first() {
            if name == "self" {
                return matches!(self_type, JophetType::MutableReference(_));
            }
        }
        false
    }

    /// Generates a C function prototype and compiles the function's body.
    ///
    /// This method generates C function signatures where struct-like types are
    /// passed by value for user-defined functions. For closures, the environment parameter
    /// is always included in the C signature to maintain a consistent calling convention,
    /// even if it's unused for zero-capture closures.
    /// It now correctly generates array parameters as pointers, and adds the `const`
    /// qualifier to non-mutating `self` parameters.
    /// It manages a stack of cleanup actions to ensure that resources are deallocated
    /// correctly before any `return` statement or at the natural end of the function.
    /// Doc comments are placed before both the prototype and the full definition.
    /// The context of the current function (its parameters, captures, and return type) is
    /// tracked to correctly compile closure bodies and `try` expressions.
    ///
    /// # Process
    /// 1. It constructs the C function prototype and adds it to `function_prototypes`.
    /// 2. It sets the generator's internal state to reflect the current function's context.
    /// 3. It temporarily swaps the main output buffer to capture the function's body in isolation.
    /// 4. It pushes a new scope onto the cleanup stack for the function body.
    /// 5. It iterates through the function's statements and compiles them into C.
    /// 6. It pops the cleanup actions for the function scope and appends them to the body.
    /// 7. It restores the main output buffer.
    /// 8. It clears the function context state.
    /// 9. It constructs the final C function definition and appends it to `function_defs`.
    ///
    /// # Panics
    /// Panics if writing to the internal buffers fails, which is not expected to happen.
    fn compile_function_declaration(&mut self, decl: &TypedFunctionDecl) {
        // Set context *before* resolving types to handle array returns correctly.
        self.current_function_return_type = Some(decl.return_type.clone());
        
        let c_return_type = self.jophet_type_to_c_return_string(&decl.return_type);
        let mut c_params = Vec::new();

        for (i, (name, ty)) in decl.params.iter().enumerate() {
            let mut param_type_str = self.jophet_type_to_c_string(ty);
            let param_name_str = self.sanitize_c_keyword(name);

            // FIX: If the parameter is an array, it must be passed by pointer.
            // C syntax for this is `base_type name[size]`, which is what the generator produces.
            let full_param_str = if matches!(ty, JophetType::Array { .. }) {
                format!("{} {}{}", self.jophet_type_to_c_string(&self.get_array_base_type(ty)), param_name_str, self.get_array_dimension_suffix(ty))
            } else {
                 // FIX: Add 'const' for read-only 'self' parameters.
                if i == 0 && name == "self" {
                    if let JophetType::Reference(inner) = ty {
                        if !self.is_primitive_for_clone(inner) {
                            if !self.is_method_mutating(decl) {
                                param_type_str = format!("const {}", param_type_str);
                            }
                        }
                    }
                }
                format!("{} {}", param_type_str, param_name_str)
            };

            c_params.push(full_param_str);
        }

        let doc_comment = self.format_doc_comment(&decl.doc_comment);

        // 1. Generate and store the prototype.
        let prototype = format!(
            "{}\n{} {}({});",
            doc_comment,
            c_return_type,
            decl.mangled_name,
            if c_params.is_empty() { "void".to_string() } else { c_params.join(", ") }
        );
        self.function_prototypes.insert(prototype.trim_start().to_string());

        // 2. Set the current function context (return type already set).
        if let Some(captures) = &decl.captures {
            self.current_closure_captures = captures.iter().map(|cap| cap.name.clone()).collect();
        }

        // 3. Swap the output buffer to compile the function body in isolation.
        let mut func_body_output = String::new();
        std::mem::swap(&mut self.output, &mut func_body_output);

        // 4. A function body is a new scope.
        self.scope_cleanup_stack.push(Vec::new());

        // 5. Compile statements.
        for s in &decl.body {
            self.compile_statement_in_function(s);
        }

        // 6. Add the end-of-scope cleanup actions before the function returns.
        let cleanup_actions = self.scope_cleanup_stack.pop().expect("Cleanup stack should not be empty at end of function");
        for action in cleanup_actions.iter().rev() {
            writeln!(&mut self.output, "\t{}", action).expect("Failed to write cleanup action");
        }

        // 7. Swap back to get the compiled body and restore the original output buffer.
        std::mem::swap(&mut self.output, &mut func_body_output);
        let func_body = func_body_output;

        // 8. Clear the function context.
        self.current_closure_captures.clear();
        self.current_function_return_type = None;

        // 9. Store the full function definition.
        let params_str = if c_params.is_empty() {
            "void".to_string()
        } else {
            c_params.join(", ")
        };
        writeln!(
            &mut self.function_defs,
            "{}\n{} {}({}) {{\n{}}}\n",
            doc_comment,
            c_return_type,
            decl.mangled_name,
            params_str,
            func_body
        )
        .expect("Failed to write to internal buffer");
    }
}