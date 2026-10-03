//! Router declarations and mounts for the line-oriented TypeScript, JavaScript, and Go extractors.
//!
//! Routes record the router they are registered on, and mounts record which router is attached
//! under which prefix. [`crate::compose_router_mounts`] resolves both per repository.

use std::collections::{BTreeMap, BTreeSet};

use crate::router_mounts::mount_observation;
use crate::source_polyglot::{Statement, quoted_values};
use crate::{SourceFramework, SourceLanguage, SourceObservation, SymbolRef};

/// Module bindings of one script file: local name to `(module specifier, imported name)`.
pub(crate) type ScriptImports = BTreeMap<String, (String, String)>;

/// Returns the identifier immediately before `.{call}(` in `text`.
pub(crate) fn receiver_before<'a>(text: &'a str, call: &str) -> Option<&'a str> {
    let at = text.find(&format!(".{call}("))?;
    let start = text[..at]
        .rfind(|character: char| !is_identifier(character))
        .map_or(0, |index| index + 1);
    let receiver = &text[start..at];
    (!receiver.is_empty()).then_some(receiver)
}

fn is_identifier(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '$'
}

/// Top-level comma-separated arguments of the first call opened at `open`.
pub(crate) fn call_arguments(text: &str, open: usize) -> Vec<&str> {
    let mut arguments = Vec::new();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut start = open + 1;
    for (index, character) in text.char_indices().skip_while(|(index, _)| *index <= open) {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None => match character {
                '\'' | '"' | '`' => quote = Some(character),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => {
                    arguments.push(text[start..index].trim());
                    return arguments
                        .into_iter()
                        .filter(|value| !value.is_empty())
                        .collect();
                }
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    arguments.push(text[start..index].trim());
                    start = index + 1;
                }
                _ => {}
            },
        }
    }
    // A statement split at a block opener, such as `r.Route("/x", func(r chi.Router) {`.
    arguments.push(text[start..].trim());
    arguments
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect()
}

fn string_literal(argument: &str) -> Option<String> {
    let first = argument.chars().next()?;
    if !matches!(first, '\'' | '"' | '`') || !argument.ends_with(first) || argument.len() < 2 {
        return None;
    }
    let value = &argument[1..argument.len() - 1];
    (!value.contains("${")).then(|| value.to_owned())
}

fn simple_identifier(value: &str) -> Option<&str> {
    (!value.is_empty() && value.chars().all(is_identifier)).then_some(value)
}

/// Discovers relative ES module and `CommonJS` bindings.
pub(crate) fn script_imports(statements: &[Statement]) -> ScriptImports {
    let mut imports = ScriptImports::new();
    for statement in statements {
        let text = statement.text.as_str();
        if let Some(rest) = text.strip_prefix("import ")
            && let Some((clause, module)) = rest.rsplit_once(" from ")
            && let Some(module) = quoted_values(module).into_iter().next()
        {
            record_import_clause(&mut imports, clause, &module);
            continue;
        }
        let Some(open) = text.find("require(") else {
            continue;
        };
        let Some(module) = quoted_values(&text[open..]).into_iter().next() else {
            continue;
        };
        let Some((left, right)) = text.split_once('=') else {
            continue;
        };
        let binding = left
            .trim()
            .trim_start_matches("const ")
            .trim_start_matches("let ")
            .trim_start_matches("var ")
            .trim();
        let member = right
            .split_once(").")
            .and_then(|(_, member)| simple_identifier(member.trim().trim_end_matches(';')));
        if let Some(fields) = binding
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        {
            for field in fields.split(',') {
                let (imported, local) = field
                    .split_once(':')
                    .map_or((field.trim(), field.trim()), |(imported, local)| {
                        (imported.trim(), local.trim())
                    });
                if let (Some(imported), Some(local)) =
                    (simple_identifier(imported), simple_identifier(local))
                {
                    imports.insert(local.to_owned(), (module.clone(), imported.to_owned()));
                }
            }
        } else if let Some(local) = simple_identifier(binding) {
            imports.insert(
                local.to_owned(),
                (module, member.unwrap_or("default").to_owned()),
            );
        }
    }
    imports.retain(|_, (module, _)| module.starts_with('.'));
    imports
}

fn record_import_clause(imports: &mut ScriptImports, clause: &str, module: &str) {
    let clause = clause.trim();
    let (default, named) = match clause.split_once('{') {
        Some((default, named)) => (default.trim().trim_end_matches(','), Some(named)),
        None => (clause, None),
    };
    if let Some(namespace) = default.trim().strip_prefix("* as ") {
        if let Some(local) = simple_identifier(namespace.trim()) {
            imports.insert(local.to_owned(), (module.to_owned(), "*".to_owned()));
        }
    } else if let Some(local) = simple_identifier(default.trim()) {
        imports.insert(local.to_owned(), (module.to_owned(), "default".to_owned()));
    }
    for item in named
        .and_then(|named| named.split('}').next())
        .into_iter()
        .flat_map(|named| named.split(','))
    {
        let item = item.trim().trim_start_matches("type ");
        let (imported, local) = item
            .split_once(" as ")
            .map_or((item, item), |(imported, local)| {
                (imported.trim(), local.trim())
            });
        if let (Some(imported), Some(local)) =
            (simple_identifier(imported), simple_identifier(local))
        {
            imports.insert(local.to_owned(), (module.to_owned(), imported.to_owned()));
        }
    }
}

/// Resolves a router expression written in a script file.
fn script_reference(
    expression: &str,
    receivers: &BTreeSet<String>,
    imports: &ScriptImports,
) -> Option<SymbolRef> {
    let expression = expression.trim().trim_end_matches("()");
    if let Some(open) = expression.find("require(") {
        let module = quoted_values(&expression[open..]).into_iter().next()?;
        let member = expression
            .split_once(").")
            .and_then(|(_, member)| simple_identifier(member))
            .unwrap_or("default");
        return module.starts_with('.').then(|| SymbolRef::Import {
            module,
            name: member.to_owned(),
        });
    }
    let (head, member) = expression
        .split_once('.')
        .map_or((expression, None), |(head, member)| (head, Some(member)));
    let head = simple_identifier(head)?;
    if let Some((module, imported)) = imports.get(head) {
        let name = match (imported.as_str(), member) {
            ("*", Some(member)) => simple_identifier(member)?.to_owned(),
            ("*", None) => "default".to_owned(),
            (imported, None) => imported.to_owned(),
            (_, Some(_)) => return None,
        };
        return Some(SymbolRef::Import {
            module: module.clone(),
            name,
        });
    }
    (member.is_none() && receivers.contains(head)).then(|| SymbolRef::Local(head.to_owned()))
}

/// Emits Express `use` mounts, default-export aliases, and router factory returns.
pub(crate) fn script_router_observations(
    statements: &[Statement],
    language: SourceLanguage,
    receivers: &BTreeSet<String>,
    imports: &ScriptImports,
) -> Vec<SourceObservation> {
    let mut output = Vec::new();
    let mut functions = Vec::<(String, i32)>::new();
    let mut depth = 0_i32;
    for statement in statements {
        let text = statement.text.as_str();
        if let Some(name) = declared_function(text) {
            functions.push((name.to_owned(), depth));
        }
        for receiver in receivers {
            let marker = format!("{receiver}.use(");
            let Some(at) = text.find(&marker) else {
                continue;
            };
            if at > 0
                && text[..at]
                    .ends_with(|character: char| is_identifier(character) || character == '.')
            {
                continue;
            }
            let arguments = call_arguments(text, at + marker.len() - 1);
            let (prefix, routers) = match arguments.split_first() {
                Some((first, rest)) => match string_literal(first) {
                    Some(prefix) => (Some(prefix), rest),
                    None => (None, arguments.as_slice()),
                },
                None => continue,
            };
            for router in routers {
                if let Some(child) = script_reference(router, receivers, imports) {
                    output.push(mount_observation(
                        language,
                        SourceFramework::Express,
                        child,
                        Some(SymbolRef::Local(receiver.clone())),
                        prefix.as_deref(),
                        statement.lines,
                    ));
                }
            }
        }
        let exported = text
            .strip_prefix("export default ")
            .or_else(|| text.strip_prefix("module.exports = "))
            .or_else(|| text.strip_prefix("module.exports="));
        if let Some(exported) =
            exported.and_then(|value| simple_identifier(value.trim().trim_end_matches(';')))
            && receivers.contains(exported)
        {
            output.push(alias(language, exported, "default", statement));
        }
        if let Some(returned) = text
            .strip_prefix("return ")
            .and_then(|value| simple_identifier(value.trim().trim_end_matches(';')))
            && receivers.contains(returned)
            && let Some((function, _)) = functions.last()
        {
            output.push(alias(language, returned, function, statement));
        }
        depth += brace_delta(text);
        while functions.last().is_some_and(|(_, opened)| depth <= *opened) {
            functions.pop();
        }
    }
    output
}

fn alias(
    language: SourceLanguage,
    router: &str,
    name: &str,
    statement: &Statement,
) -> SourceObservation {
    mount_observation(
        language,
        SourceFramework::Express,
        SymbolRef::Local(router.to_owned()),
        Some(SymbolRef::Local(name.to_owned())),
        None,
        statement.lines,
    )
}

fn declared_function(text: &str) -> Option<&str> {
    let text = text
        .trim_start_matches("export ")
        .trim_start_matches("default ")
        .trim_start_matches("async ");
    if let Some(rest) = text.strip_prefix("function ") {
        return simple_identifier(rest.split('(').next()?.trim());
    }
    let rest = text
        .strip_prefix("const ")
        .or_else(|| text.strip_prefix("let "))?;
    let (name, value) = rest.split_once('=')?;
    (value.contains("=>") && text.trim_end().ends_with('{'))
        .then(|| simple_identifier(name.trim()))
        .flatten()
}

/// Net `{` minus `}` outside string literals.
pub(crate) fn brace_delta(text: &str) -> i32 {
    let mut quote = None;
    let mut delta = 0;
    let mut previous = '\0';
    for character in text.chars() {
        match quote {
            Some(active) if character == active && previous != '\\' => quote = None,
            None if matches!(character, '\'' | '"' | '`') => quote = Some(character),
            None if character == '{' => delta += 1,
            None if character == '}' => delta -= 1,
            Some(_) | None => {}
        }
        previous = character;
    }
    delta
}

const GO_ROUTER_TYPES: [&str; 7] = [
    "*gin.Engine",
    "*gin.RouterGroup",
    "gin.IRouter",
    "gin.IRoutes",
    "chi.Router",
    "*chi.Mux",
    "*http.ServeMux",
];

const GO_ROUTER_FACTORIES: [&str; 5] = [
    "gin.Default(",
    "gin.New(",
    "chi.NewRouter(",
    "chi.NewMux(",
    "http.NewServeMux(",
];

/// Lexical router scope of one Go statement.
#[derive(Debug, Default)]
pub(crate) struct GoRouterScope {
    function: Option<String>,
    parameters: BTreeSet<String>,
    frames: Vec<(String, String, i32)>,
    package_routers: BTreeSet<String>,
    function_routers: BTreeSet<String>,
    depth: i32,
}

impl GoRouterScope {
    /// Advances the scope past `statement` and returns the mounts it declares.
    pub(crate) fn observe(
        &mut self,
        statement: &Statement,
        framework: SourceFramework,
    ) -> Vec<SourceObservation> {
        let text = statement.text.as_str();
        if self.depth == 0
            && let Some((name, parameters)) = go_function_signature(text)
        {
            self.function = Some(name);
            self.parameters = parameters;
            self.function_routers.clear();
        }
        let mut output = Vec::new();
        let mount = |child: SymbolRef, parent: SymbolRef, prefix: Option<&str>| {
            mount_observation(
                SourceLanguage::Go,
                framework,
                child,
                Some(parent),
                prefix,
                statement.lines,
            )
        };
        if let Some((assigned, value)) = go_assignment(text) {
            let is_router = GO_ROUTER_FACTORIES
                .iter()
                .any(|factory| value.contains(factory));
            let group = self.receiver_call(value, "Group");
            if is_router || group.is_some() {
                if self.function.is_some() && self.depth > 0 {
                    self.function_routers.insert(assigned.to_owned());
                } else {
                    self.package_routers.insert(assigned.to_owned());
                }
            }
            if let Some((parent, arguments)) = group {
                let prefix = arguments
                    .first()
                    .and_then(|argument| string_literal(argument));
                output.push(mount(
                    self.reference(assigned),
                    self.reference(parent),
                    prefix.as_deref(),
                ));
            }
        }
        output.extend(self.closure_frame(statement, framework));
        if let Some((parent, arguments)) = self.receiver_call(text, "Mount")
            && let [prefix, child] = arguments.as_slice()
            && let Some(prefix) = string_literal(prefix)
            && let Some(child) = self.expression_reference(child)
        {
            output.push(mount(child, self.reference(parent), Some(&prefix)));
        }
        if let Some((parent, arguments)) = self.receiver_call(text, "Handle")
            && let [_, handler] = arguments.as_slice()
            && let Some(open) = handler.find("http.StripPrefix(")
            && let [prefix, child] =
                call_arguments(handler, open + "http.StripPrefix".len()).as_slice()
            && let Some(prefix) = string_literal(prefix)
            && let Some(child) = self.expression_reference(child)
        {
            output.push(mount(child, self.reference(parent), Some(&prefix)));
        }
        output.extend(self.function_call_mounts(text, framework, statement));
        if let Some(returned) = text.strip_prefix("return ").and_then(simple_identifier)
            && self.is_router(returned)
            && let Some(function) = &self.function
        {
            output.push(mount(
                self.reference(returned),
                SymbolRef::Function(function.clone()),
                None,
            ));
        }
        self.depth += brace_delta(text);
        self.frames.retain(|(_, _, depth)| self.depth >= *depth);
        if self.depth <= 0 {
            self.depth = 0;
            self.function = None;
            self.parameters.clear();
            self.function_routers.clear();
        }
        output
    }

    /// Opens a Chi `Route` or `Group` closure frame and returns its mount.
    fn closure_frame(
        &mut self,
        statement: &Statement,
        framework: SourceFramework,
    ) -> Option<SourceObservation> {
        let text = statement.text.as_str();
        let variable = go_closure_router(text)?;
        let (parent, prefix) = if let Some((parent, arguments)) = self.receiver_call(text, "Route")
        {
            let prefix = arguments
                .first()
                .and_then(|argument| string_literal(argument))?;
            (parent, Some(prefix))
        } else {
            (self.receiver_call(text, "Group")?.0, None)
        };
        let key = self.frame_key(&variable, statement);
        let observation = mount_observation(
            SourceLanguage::Go,
            framework,
            SymbolRef::Local(key.clone()),
            Some(self.reference(parent)),
            prefix.as_deref(),
            statement.lines,
        );
        self.frames.push((variable, key, self.depth + 1));
        Some(observation)
    }

    /// Router registered by `receiver` in the current scope.
    pub(crate) fn reference(&self, receiver: &str) -> SymbolRef {
        if let Some((_, key, _)) = self
            .frames
            .iter()
            .rev()
            .find(|(variable, _, _)| variable == receiver)
        {
            return SymbolRef::Local(key.clone());
        }
        match &self.function {
            Some(function) if self.parameters.contains(receiver) => {
                SymbolRef::Function(function.clone())
            }
            Some(function) if !self.package_routers.contains(receiver) => {
                SymbolRef::Local(format!("{function}.{receiver}"))
            }
            _ => SymbolRef::Local(receiver.to_owned()),
        }
    }

    fn is_router(&self, name: &str) -> bool {
        self.parameters.contains(name)
            || self.function_routers.contains(name)
            || self.package_routers.contains(name)
            || self.frames.iter().any(|(variable, _, _)| variable == name)
    }

    fn frame_key(&self, variable: &str, statement: &Statement) -> String {
        format!(
            "{}.{variable}@{}",
            self.function.as_deref().unwrap_or_default(),
            statement.lines.start
        )
    }

    fn receiver_call<'t>(&self, text: &'t str, call: &str) -> Option<(&'t str, Vec<&'t str>)> {
        let receiver = receiver_before(text, call)?;
        if !self.is_router(receiver) {
            return None;
        }
        let open = text.find(&format!("{receiver}.{call}("))? + receiver.len() + call.len() + 1;
        Some((receiver, call_arguments(text, open)))
    }

    fn expression_reference(&self, expression: &str) -> Option<SymbolRef> {
        let expression = expression.trim();
        if let Some(name) = simple_identifier(expression) {
            return self.is_router(name).then(|| self.reference(name));
        }
        let callee = expression.strip_suffix("()")?;
        callee
            .split('.')
            .all(|segment| simple_identifier(segment).is_some())
            .then(|| SymbolRef::Call(callee.to_owned()))
    }

    /// Mounts a router function called with a router argument, such as `routes.Register(v1)`.
    fn function_call_mounts(
        &self,
        text: &str,
        framework: SourceFramework,
        statement: &Statement,
    ) -> Vec<SourceObservation> {
        let Some(open) = text.find('(') else {
            return Vec::new();
        };
        let callee = text[..open].trim();
        if callee.is_empty()
            || callee.starts_with("func")
            || !callee
                .split('.')
                .all(|segment| simple_identifier(segment).is_some())
            || callee
                .split('.')
                .next()
                .is_some_and(|head| self.is_router(head))
        {
            return Vec::new();
        }
        call_arguments(text, open)
            .into_iter()
            .filter_map(|argument| {
                if let Some(name) = simple_identifier(argument) {
                    return self.is_router(name).then(|| (self.reference(name), None));
                }
                let (parent, arguments) = self.receiver_call(argument, "Group")?;
                Some((
                    self.reference(parent),
                    arguments
                        .first()
                        .and_then(|argument| string_literal(argument)),
                ))
            })
            .map(|(parent, prefix)| {
                mount_observation(
                    SourceLanguage::Go,
                    framework,
                    SymbolRef::Call(callee.to_owned()),
                    Some(parent),
                    prefix.as_deref(),
                    statement.lines,
                )
            })
            .collect()
    }
}

fn go_function_signature(text: &str) -> Option<(String, BTreeSet<String>)> {
    let rest = text.strip_prefix("func ")?;
    let rest = if rest.starts_with('(') {
        rest.split_once(')')?.1.trim_start()
    } else {
        rest
    };
    let (name, after) = rest.split_once('(')?;
    let name = simple_identifier(name.trim())?;
    let parameters = after.split(')').next().unwrap_or_default();
    let mut routers = BTreeSet::new();
    let mut pending = Vec::new();
    for parameter in parameters.split(',') {
        let mut parts = parameter.split_whitespace();
        let Some(variable) = parts.next() else {
            continue;
        };
        pending.push(variable.to_owned());
        if let Some(kind) = parts.next() {
            if GO_ROUTER_TYPES.contains(&kind) {
                routers.extend(pending.drain(..));
            } else {
                pending.clear();
            }
        }
    }
    Some((name.to_owned(), routers))
}

fn go_assignment(text: &str) -> Option<(&str, &str)> {
    let (left, right) = text.split_once(":=").or_else(|| {
        let (left, right) = text.split_once('=')?;
        (!left.ends_with(['!', '<', '>', '='])).then_some((left, right))
    })?;
    let left = left.trim().trim_start_matches("var ").trim();
    Some((simple_identifier(left)?, right.trim()))
}

fn go_closure_router(text: &str) -> Option<String> {
    let rest = &text[text.find("func(")? + "func(".len()..];
    let mut parts = rest.split(')').next()?.split_whitespace();
    let variable = simple_identifier(parts.next()?)?;
    parts
        .next()
        .is_some_and(|kind| GO_ROUTER_TYPES.contains(&kind))
        .then(|| variable.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{call_arguments, receiver_before};

    #[test]
    fn call_arguments_should_split_top_level_arguments_only() {
        let text = "app.use('/api', auth({ a: 1, b: [2, 3] }), require('./x'))";
        let open = text.find('(').unwrap_or_default();

        assert_eq!(
            call_arguments(text, open),
            ["'/api'", "auth({ a: 1, b: [2, 3] })", "require('./x')"]
        );
        assert_eq!(receiver_before("  v1.GET(\"/x\", h)", "GET"), Some("v1"));
    }
}
