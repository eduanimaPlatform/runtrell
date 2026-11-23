// SPDX-License-Identifier: Apache-2.0
use std::{collections::BTreeMap, fs, path::Path};
use syn::{
    Expr, Item,
    visit::{self, Visit},
};

#[derive(Default)]
struct Sites {
    function: String,
    sites: BTreeMap<(String, String), Vec<usize>>,
    errors: Vec<String>,
}

impl<'ast> Visit<'ast> for Sites {
    fn visit_item(&mut self, item: &'ast Item) {
        if let Item::Fn(function) = item {
            let previous = std::mem::replace(&mut self.function, function.sig.ident.to_string());
            visit::visit_item(self, item);
            self.function = previous;
        } else {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        let previous = std::mem::replace(&mut self.function, function.sig.ident.to_string());
        visit::visit_impl_item_fn(self, function);
        self.function = previous;
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let Expr::Path(path) = call.func.as_ref()
            && let Some(name) = path.path.segments.last()
            && name.ident == "ambiguous"
        {
            let line = name.ident.span().start().line;
            let reason = match call.args.iter().nth(1) {
                Some(Expr::Path(path))
                    if path.path.segments.len() == 2
                        && path.path.segments[0].ident == "AmbiguityReason" =>
                {
                    path.path.segments.last().unwrap().ident.to_string()
                }
                _ => {
                    self.errors.push(format!(
                        "{line}: use a closed AmbiguityReason at the constructor call"
                    ));
                    return;
                }
            };
            self.sites
                .entry((self.function.clone(), reason))
                .or_default()
                .push(line);
            for arg in &call.args {
                self.visit_expr(arg);
            }
        } else {
            visit::visit_expr_call(self, call);
        }
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if path.path.segments.len() > 1
            && let Some(name) = path.path.segments.last()
            && name.ident == "ambiguous"
        {
            self.errors.push(format!(
                "{}: call the ambiguity constructor directly",
                name.ident.span().start().line
            ));
        }
        visit::visit_expr_path(self, path);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        let ident = match tree {
            syn::UseTree::Name(name) => Some(&name.ident),
            syn::UseTree::Rename(rename) => Some(&rename.ident),
            _ => None,
        };
        if let Some(ident) = ident
            && ident == "ambiguous"
        {
            self.errors.push(format!(
                "{}: do not alias the ambiguity constructor",
                ident.span().start().line
            ));
        }
        visit::visit_use_tree(self, tree);
    }

    fn visit_expr_struct(&mut self, expression: &'ast syn::ExprStruct) {
        if let Some(name) = expression.path.segments.last()
            && name.ident == "AmbiguousOutcome"
            && self.function != "ambiguous"
        {
            self.errors.push(format!(
                "{}: construct ambiguity through ExternalResult::ambiguous",
                name.ident.span().start().line
            ));
        }
        visit::visit_expr_struct(self, expression);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // A macro can hide an invocation from the Rust AST of its caller.
        if constructor_token(mac.tokens.clone()) {
            self.errors.push(format!(
                "{}: ambiguity construction must be visible in Rust source",
                mac.path.segments[0].ident.span().start().line
            ));
        }
        visit::visit_macro(self, mac);
    }
}

fn constructor_token(tokens: proc_macro2::TokenStream) -> bool {
    use proc_macro2::{Delimiter, TokenTree};
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let punct =
        |token: &TokenTree| matches!(token, TokenTree::Punct(punct) if punct.as_char() == ':');
    tokens.windows(3).any(|tokens| {
        punct(&tokens[0]) && punct(&tokens[1])
            && matches!(&tokens[2], TokenTree::Ident(ident) if ident == "ambiguous")
    }) || tokens.windows(2).any(|tokens| {
        matches!(&tokens[0], TokenTree::Ident(ident) if ident == "AmbiguousOutcome")
            && matches!(&tokens[1], TokenTree::Group(group) if group.delimiter() == Delimiter::Brace)
    }) || tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => constructor_token(group.stream()),
        _ => false,
    })
}

fn load(
    root: &Path,
    relative: &Path,
    found: &mut BTreeMap<String, Vec<usize>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(relative);
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            load(root, &relative.join(entry?.file_name()), found)?;
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        let mut sites = Sites::default();
        sites.visit_file(&syn::parse_file(&fs::read_to_string(&path)?)?);
        if !sites.errors.is_empty() {
            return Err(format!("{}: {}", relative.display(), sites.errors.join("\n")).into());
        }
        for ((function, reason), lines) in sites.sites {
            found.insert(format!("{} {function} {reason}", relative.display()), lines);
        }
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut found = BTreeMap::new();
    load(root, Path::new("crates"), &mut found)?;
    let mut approved = BTreeMap::new();
    for line in include_str!("ambiguity-sites.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let (key, count) = line
            .rsplit_once(' ')
            .ok_or("invalid ambiguity allowlist row")?;
        let count: usize = count.parse()?;
        if count == 0 || approved.insert(key, count).is_some() {
            return Err(format!("invalid ambiguity allowlist row: {line}").into());
        }
    }

    let mut differences = Vec::new();
    for (key, lines) in &found {
        if approved.remove(key.as_str()) != Some(lines.len()) {
            differences.push(format!("{key} {} (lines {lines:?})", lines.len()));
        }
    }
    differences.extend(approved.keys().map(|key| format!("missing {key}")));
    if !differences.is_empty() {
        return Err(format!("ambiguity allowlist mismatch:\n{}", differences.join("\n")).into());
    }
    println!(
        "Verified {} ambiguity construction sites",
        found.values().map(Vec::len).sum::<usize>()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sites_and_constructor_aliases_are_detected() {
        let code = r#"
            use R::ambiguous as unsafe_alias;
            fn fixture() {
                let ambiguous = R::ambiguous(Evidence::Sent, AmbiguityReason::ResponseLost);
                consume(ambiguous);
                let _ = R::ambiguous;
                let _ = R::ambiguous(Evidence::Sent, AmbiguityReason::ResponseLost);
            }
        "#;
        let mut sites = Sites::default();
        sites.visit_file(&syn::parse_file(code).unwrap());
        assert_eq!(sites.sites.values().next().unwrap().len(), 2);
        assert_eq!(sites.errors.len(), 2);
    }
}
