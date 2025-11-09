// src/docs/html_generator.rs
//! Implements the HTML documentation generator for Jophet.
//!
//! This module takes the final `AnalysisResult` from the semantic analyzer
//! and builds a single, self-contained HTML file documenting the public API
//! of the project. It renders doc comments as Markdown and provides syntax
//! highlighting for code examples.

use crate::core::ast::typed::{
    JophetType, TypedEnumDef, TypedErrorDef, TypedFunctionDecl, TypedProgram, TypedStatementKind,
    TypedStructDef, TypedTaggedUnionDef, TypedUnionDef,
};
use crate::core::semantic_analyzer::types::jophet_type_to_user_string;
use crate::core::semantic_analyzer::AnalysisResult;
use pulldown_cmark::{html, Options, Parser};
use std::error::Error;
use std::fmt::Write;

/// The main entry point for the HTML generator.
pub fn generate_html(
    package_name: &str,
    analysis_result: &AnalysisResult,
) -> Result<String, Box<dyn Error>> {
    let mut gen = Generator::new(package_name, analysis_result);
    gen.write_document()?;
    Ok(gen.html)
}

/// A helper struct to manage the state of HTML generation.
struct Generator<'a> {
    html: String,
    package_name: &'a str,
    analysis_result: &'a AnalysisResult,
}

/// Helper to find a fully typed function declaration from the final program.
fn find_method_in_program<'b>(program: &'b TypedProgram, mangled_name: &str) -> Option<&'b TypedFunctionDecl> {
    program.iter().find_map(|stmt| {
        if let TypedStatementKind::FunctionDecl(decl) = &stmt.kind {
            if decl.mangled_name == mangled_name {
                return Some(decl);
            }
        }
        None
    })
}

impl<'a> Generator<'a> {
    /// Creates a new HTML generator.
    fn new(package_name: &'a str, analysis_result: &'a AnalysisResult) -> Self {
        Self {
            html: String::new(),
            package_name,
            analysis_result,
        }
    }

    /// Renders a Markdown string into an HTML string.
    fn render_markdown(&self, text: &str) -> String {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TABLES);
        let parser = Parser::new_ext(text, options);
        let mut html_output = String::new();
        html::push_html(&mut html_output, parser);
        html_output
    }

    /// Generates the entire HTML document.
    fn write_document(&mut self) -> Result<(), Box<dyn Error>> {
        self.write_header()?;
        self.write_body_start()?;
        self.write_sidebar()?;
        self.write_main_content()?;
        self.write_body_end()?;
        Ok(())
    }

    /// Writes the HTML head, including CSS and JavaScript for syntax highlighting.
    fn write_header(&mut self) -> Result<(), Box<dyn Error>> {
        writeln!(
            self.html,
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Documentation for {}</title>
    <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.9.0/styles/github.min.css">
    <script src="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.9.0/highlight.min.js"></script>
    <script>document.addEventListener('DOMContentLoaded', (event) => {{ document.querySelectorAll('pre code').forEach((el) => {{ hljs.highlightElement(el); }}); }});</script>
    <style>
        :root {{
            --main-bg-color: #ffffff;
            --text-color: #24292e;
            --sidebar-bg: #f6f8fa;
            --border-color: #e1e4e8;
            --link-color: #0366d6;
            --code-bg: #f6f8fa;
            --pre-bg: #f6f8fa;
            --accent-color: #0366d6;
        }}
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; line-height: 1.6; color: var(--text-color); margin: 0; background-color: var(--main-bg-color); }}
        .container {{ display: flex; }}
        #sidebar {{ width: 260px; height: 100vh; position: fixed; top: 0; left: 0; background-color: var(--sidebar-bg); border-right: 1px solid var(--border-color); padding: 1.5rem; overflow-y: auto; }}
        #main-content {{ margin-left: 260px; padding: 2rem 3rem; max-width: 960px; }}
        h1, h2, h3, h4 {{ color: #1a1a1a; }}
        h1 {{ font-size: 2em; border-bottom: 1px solid var(--border-color); padding-bottom: 0.5rem; }}
        h2 {{ font-size: 1.75em; margin-top: 2.5rem; border-bottom: 1px solid var(--border-color); padding-bottom: 0.5rem; }}
        h3 {{ font-size: 1.4em; margin-top: 2rem; }}
        #sidebar h1 {{ font-size: 1.5em; margin-top: 0; }}
        #sidebar h1 > code {{ padding: 0; background-color: transparent; }}
        #sidebar ul {{ list-style: none; padding: 0; }}
        #sidebar ul a {{ text-decoration: none; color: var(--link-color); display: block; padding: 0.2rem 0; font-size: 0.95em; }}
        #sidebar ul a:hover {{ text-decoration: underline; }}
        #sidebar h2 {{ font-size: 1.1em; margin-top: 1.5rem; color: var(--text-color); border: none; }}
        code, pre {{ font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, Courier, monospace; }}
        code {{ background-color: var(--code-bg); border-radius: 6px; font-size: 0.9em; padding: 0.2em 0.4em; }}
        pre {{ padding: 1rem; overflow-x: auto; background-color: var(--pre-bg); border: 1px solid var(--border-color); border-radius: 6px; }}
        pre > code {{ padding: 0; background-color: transparent; border-radius: 0; }}
        .item {{ margin-bottom: 2rem; margin-left: 1.5rem; }}
        #main-content > h1, #main-content > h2, #main-content > .doc-block {{ margin-left: 1.5rem; }}
        .item-header {{ border-bottom: 1px solid var(--border-color); padding-bottom: 0.5rem; }}
        .item-details {{ margin-top: 1rem; }}
        .doc-block {{ margin-top: 1rem; color: #444; }}
        .doc-block p:first-child {{ margin-top: 0; }}
        .doc-block p:last-child {{ margin-bottom: 0; }}
        .fields-table {{ width: 100%; border-collapse: collapse; margin-top: 1rem; }}
        .fields-table th, .fields-table td {{ text-align: left; padding: 0.5rem; border: 1px solid var(--border-color); }}
        .fields-table th {{ background-color: var(--sidebar-bg); }}
    </style>
</head>"#,
            self.package_name
        )?;
        Ok(())
    }

    /// Writes the opening tags for the main layout.
    fn write_body_start(&mut self) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "<body>\n<div class='container'>")?;
        Ok(())
    }

    /// Writes the closing tags for the main layout.
    fn write_body_end(&mut self) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "</div>\n</body>\n</html>")?;
        Ok(())
    }

    /// Generates and writes the entire navigation sidebar.
    fn write_sidebar(&mut self) -> Result<(), Box<dyn Error>> {
        let scope = &self.analysis_result.public_scope;
        writeln!(self.html, "<nav id='sidebar'>")?;
        writeln!(self.html, "<h1><code>{}</code></h1>", self.package_name)?;

        if !scope.struct_defs.is_empty() {
            writeln!(self.html, "<h2>Structs</h2><ul>")?;
            for name in scope.struct_defs.keys() {
                writeln!(self.html, "<li><a href='#struct.{}'>{}</a></li>", name, name)?;
            }
            writeln!(self.html, "</ul>")?;
        }
        if !scope.enum_defs.is_empty() {
            writeln!(self.html, "<h2>Enums</h2><ul>")?;
            for name in scope.enum_defs.keys() {
                writeln!(self.html, "<li><a href='#enum.{}'>{}</a></li>", name, name)?;
            }
            writeln!(self.html, "</ul>")?;
        }
        if !scope.union_defs.is_empty() {
            writeln!(self.html, "<h2>Unions</h2><ul>")?;
            for name in scope.union_defs.keys() {
                writeln!(self.html, "<li><a href='#union.{}'>{}</a></li>", name, name)?;
            }
            writeln!(self.html, "</ul>")?;
        }
        if !scope.tagged_union_defs.is_empty() {
            writeln!(self.html, "<h2>Tagged Unions</h2><ul>")?;
            for name in scope.tagged_union_defs.keys() {
                writeln!(self.html, "<li><a href='#tagged_union.{}'>{}</a></li>", name, name)?;
            }
            writeln!(self.html, "</ul>")?;
        }
        if !scope.error_defs.is_empty() {
            writeln!(self.html, "<h2>Errors</h2><ul>")?;
            for name in scope.error_defs.keys() {
                writeln!(self.html, "<li><a href='#error.{}'>{}</a></li>", name, name)?;
            }
            writeln!(self.html, "</ul>")?;
        }
        if !scope.symbol_table.is_empty() {
            writeln!(self.html, "<h2>Functions</h2><ul>")?;
            for name in scope.symbol_table.keys() {
                writeln!(self.html, "<li><a href='#function.{}'>{}</a></li>", name, name)?;
            }
            writeln!(self.html, "</ul>")?;
        }

        writeln!(self.html, "</nav>")?;
        Ok(())
    }

    /// Generates and writes the main content area with all documentation items.
    fn write_main_content(&mut self) -> Result<(), Box<dyn Error>> {
        let scope = &self.analysis_result.public_scope;
        writeln!(self.html, "<main id='main-content'>")?;
        writeln!(self.html, "<h1>Package <code>{}</code></h1>", self.package_name)?;
        if let Some(doc) = &self.analysis_result.module_doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }

        if !scope.struct_defs.is_empty() {
            writeln!(self.html, "<h2>Structs</h2>")?;
            for def in scope.struct_defs.values() {
                self.write_struct_doc(def)?;
            }
        }
        if !scope.enum_defs.is_empty() {
            writeln!(self.html, "<h2>Enums</h2>")?;
            for def in scope.enum_defs.values() {
                self.write_enum_doc(def)?;
            }
        }
        if !scope.union_defs.is_empty() {
            writeln!(self.html, "<h2>Unions</h2>")?;
            for def in scope.union_defs.values() {
                self.write_union_doc(def)?;
            }
        }
        if !scope.tagged_union_defs.is_empty() {
            writeln!(self.html, "<h2>Tagged Unions</h2>")?;
            for def in scope.tagged_union_defs.values() {
                self.write_tagged_union_doc(def)?;
            }
        }
        if !scope.error_defs.is_empty() {
            writeln!(self.html, "<h2>Errors</h2>")?;
            for def in scope.error_defs.values() {
                self.write_error_doc(def)?;
            }
        }
        if !scope.symbol_table.is_empty() {
            writeln!(self.html, "<h2>Functions</h2>")?;
            let functions: Vec<_> = self.analysis_result.typed_program.iter().filter_map(|stmt| {
                if let TypedStatementKind::FunctionDecl(decl) = &stmt.kind {
                    if decl.is_public && decl.receiver_type.is_none() { Some(decl) } else { None }
                } else { None }
            }).collect();
            for func in functions {
                self.write_function_doc(func)?;
            }
        }
        
        writeln!(self.html, "</main>")?;
        Ok(())
    }

    fn write_struct_doc(&mut self, def: &TypedStructDef) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "<div class='item' id='struct.{}'>", def.name)?;
        writeln!(self.html, "<div class='item-header'><h3><code>struct {}</code></h3></div>", def.name)?;
        if let Some(doc) = &def.doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }
        writeln!(self.html, "<div class='item-details'><h4>Fields</h4>")?;
        writeln!(self.html, "<table class='fields-table'><thead><tr><th>Name</th><th>Type</th><th>Description</th></tr></thead><tbody>")?;
        for (name, ty, is_public) in &def.fields {
            if *is_public {
                let type_str = jophet_type_to_user_string(ty);
                let field_doc = ""; // Field doc comments aren't currently stored on TypedStructDef, a future improvement.
                writeln!(self.html, "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>", name, type_str, field_doc)?;
            }
        }
        writeln!(self.html, "</tbody></table></div>")?;

        // Find and render methods for this struct
        if let Some(methods) = self.analysis_result.public_scope.method_defs.get(&def.name) {
             if !methods.is_empty() {
                writeln!(self.html, "<div class='item-details'><h4>Methods</h4>")?;
                 for method in methods.values() {
                     let func_decl = find_method_in_program(&self.analysis_result.typed_program, &method.mangled_name).unwrap();
                     self.write_function_doc(func_decl)?;
                 }
                writeln!(self.html, "</div>")?;
             }
        }
        
        writeln!(self.html, "</div>")?;
        Ok(())
    }

    fn write_enum_doc(&mut self, def: &TypedEnumDef) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "<div class='item' id='enum.{}'>", def.name)?;
        writeln!(self.html, "<div class='item-header'><h3><code>enum {}</code></h3></div>", def.name)?;
        if let Some(doc) = &def.doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }
        writeln!(self.html, "<div class='item-details'><h4>Members</h4><pre><code>")?;
        for (name, val, _doc) in &def.members {
            writeln!(self.html, "{} = {}", name, val)?;
        }
        writeln!(self.html, "</code></pre></div></div>")?;
        Ok(())
    }

    fn write_union_doc(&mut self, def: &TypedUnionDef) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "<div class='item' id='union.{}'>", def.name)?;
        writeln!(self.html, "<div class='item-header'><h3><code>union {}</code></h3></div>", def.name)?;
        if let Some(doc) = &def.doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }
        writeln!(self.html, "<div class='item-details'><h4>Fields</h4><pre><code>")?;
        for (name, ty, _doc) in &def.fields {
            writeln!(self.html, "{}: {}", name, jophet_type_to_user_string(ty))?;
        }
        writeln!(self.html, "</code></pre></div></div>")?;
        Ok(())
    }

    fn write_tagged_union_doc(&mut self, def: &TypedTaggedUnionDef) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "<div class='item' id='tagged_union.{}'>", def.name)?;
        writeln!(self.html, "<div class='item-header'><h3><code>tagged union {}</code></h3></div>", def.name)?;
        if let Some(doc) = &def.doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }
        writeln!(self.html, "<div class='item-details'><h4>Variants</h4><pre><code>")?;
        for variant in &def.variants {
            if let Some(payload) = &variant.payload {
                writeln!(self.html, "{}({})", variant.name, jophet_type_to_user_string(payload))?;
            } else {
                writeln!(self.html, "{}", variant.name)?;
            }
        }
        writeln!(self.html, "</code></pre></div></div>")?;
        Ok(())
    }

    fn write_error_doc(&mut self, def: &TypedErrorDef) -> Result<(), Box<dyn Error>> {
        writeln!(self.html, "<div class='item' id='error.{}'>", def.name)?;
        writeln!(self.html, "<div class='item-header'><h3><code>error {}</code></h3></div>", def.name)?;
        if let Some(doc) = &def.doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }
        writeln!(self.html, "<div class='item-details'><h4>Variants</h4><pre><code>")?;
        for variant in &def.variants {
            if let Some(payload) = &variant.payload {
                writeln!(self.html, "{}({})", variant.name, jophet_type_to_user_string(payload))?;
            } else {
                writeln!(self.html, "{}", variant.name)?;
            }
        }
        writeln!(self.html, "</code></pre></div></div>")?;
        Ok(())
    }

    fn write_function_doc(&mut self, decl: &TypedFunctionDecl) -> Result<(), Box<dyn Error>> {
        let params: String = decl.params.iter()
            .map(|(name, ty)| format!("{}: {}", name, jophet_type_to_user_string(ty)))
            .collect::<Vec<_>>().join(", ");
        let return_type = if decl.return_type != JophetType::Nothing {
            format!(": {}", jophet_type_to_user_string(&decl.return_type))
        } else { "".to_string() };
        let signature = format!("public function {}({}){}", decl.name, params, return_type);

        writeln!(self.html, "<div class='item' id='function.{}'>", decl.name)?;
        writeln!(self.html, "<div class='item-header'><h3><code>{}</code></h3></div>", decl.name)?;
        if let Some(doc) = &decl.doc_comment {
            writeln!(self.html, "<div class='doc-block'>{}</div>", self.render_markdown(doc))?;
        }
        writeln!(self.html, "<pre><code class='language-jophet'>{}</code></pre>", signature)?;
        writeln!(self.html, "</div>")?;
        Ok(())
    }
}