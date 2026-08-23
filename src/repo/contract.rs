use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSpec {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredMethod {
    pub file: Option<String>,
    /// Owning type when the requirement names one explicitly (`User::new`).
    pub owner: Option<String>,
    pub fn_name: String,
    pub raw_signature: String,
    /// Receiver when the requirement is a method (`&self`, `&mut self`, ...).
    pub self_kind: Option<String>,
    pub params: Vec<ParamSpec>,
    pub return_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskContract {
    pub methods: Vec<RequiredMethod>,
    pub type_constraints: Vec<String>,
    /// Verbatim behavioral requirements extracted from the task (preconditions,
    /// valid/invalid state transitions, postconditions). Rendered into every
    /// prompt and used to synthesize the locked acceptance oracle.
    pub behavior_notes: Vec<String>,
}

impl TaskContract {
    /// Extracts explicit method signatures and type constraints from task descriptions.
    pub fn extract(task: &str) -> Self {
        let mut methods = Vec::new();
        let mut type_constraints = Vec::new();
        let mut behavior_notes = Vec::new();

        for line in task.lines() {
            let trimmed = line.trim();
            for seg in trimmed.split([';', '.', '!', '?']) {
                let seg = seg.trim();
                if is_behavior_note(seg) && behavior_notes.len() < 16 {
                    behavior_notes.push(seg.to_string());
                }
            }
            // Look for patterns like `metodo transfer(&mut self, from_id: u64, to_id: u64, amount: f64) -> Result<(), String>`
            // or `fn transfer(...)`
            if let Some(fn_start) = trimmed.find("(&mut self")
                .or_else(|| trimmed.find("(&self"))
                .or_else(|| trimmed.find("(self"))
                .or_else(|| trimmed.find("(mut self"))
            {
                // Find method name before '('
                let before = &trimmed[..fn_start];
                let name_token = before
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .trim_start_matches("metodo ")
                    .trim_start_matches("fn ")
                    .trim_matches('`')
                    .trim();
                // `User::register(&mut self, ...)` carries an explicit owner.
                let (owner, fn_name) = match name_token.rsplit_once("::") {
                    Some((o, n)) => (Some(o.to_string()), n),
                    None => (None, name_token),
                };

                if !fn_name.is_empty() {
                    let after = &trimmed[fn_start..];
                    let sig_end = after.find(')').map(|i| i + 1).unwrap_or(after.len());
                    let mut full_sig = format!("{fn_name}{}", &after[..sig_end]);
                    let receiver_part = &after[1..sig_end - 1];
                    let self_kind = receiver_part
                        .split(',')
                        .next()
                        .map(str::trim)
                        .and_then(crate::repo::spec::SelfKind::parse)
                        .map(|sk| sk.as_str().to_string());

                    // Extract return type if present
                    let ret_part = &after[sig_end..];
                    let return_type = if let Some(arrow_idx) = ret_part.find("->") {
                        let ret_after = &ret_part[arrow_idx + 2..];
                        let clean_ret = ret_after
                            .split_whitespace()
                            .take_while(|s| !s.contains("que") && !s.contains("validando") && !s.contains("chamando") && !s.ends_with('.'))
                            .collect::<Vec<_>>()
                            .join(" ");
                        let clean_ret = clean_ret.trim_end_matches('.').trim().to_string();
                        if !clean_ret.is_empty() {
                            full_sig.push_str(&format!(" -> {clean_ret}"));
                            Some(clean_ret)
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Extract parameter types
                    let mut params = Vec::new();
                    let param_str = &after[1..sig_end - 1]; // inside parentheses
                    for part in param_str.split(',') {
                        let p = part.trim();
                        if p.starts_with('&') || p == "self" || p == "mut self" {
                            continue;
                        }
                        if let Some((pname, ptype)) = p.split_once(':') {
                            params.push(ParamSpec {
                                name: pname.trim().to_string(),
                                type_name: ptype.trim().to_string(),
                            });
                        }
                    }

                    // Identify target file from line context
                    let file = detect_target_file(line);

                    methods.push(RequiredMethod {
                        file,
                        owner,
                        fn_name: fn_name.to_string(),
                        raw_signature: full_sig,
                        self_kind,
                        params,
                        return_type,
                    });
                }
            } else if let Some(req) = extract_associated_fn(trimmed) {
                // Associated-function style requirements such as
                // `User::new(name: String, age: u32) -> User`.
                methods.push(req);
            }
        }

        // Add explicit type constraints based on extracted parameters
        let mut seen_params = HashMap::new();
        for m in &methods {
            for p in &m.params {
                seen_params.entry(p.name.clone()).or_insert_with(Vec::new).push(p.type_name.clone());
            }
        }

        for (pname, types) in seen_params {
            // Only emit an invariant when the requirement is consistent.
            let all_same = types.iter().all(|t| t == &types[0]);
            if !all_same {
                continue; // conflicting requirement; do not invent an invariant
            }
            if let Some(first_type) = types.first() {
                type_constraints.push(format!("`{pname}` must be type `{first_type}` (never change to another numeric/primitive type)."));
            }
        }

        Self {
            methods,
            type_constraints,
            behavior_notes,
        }
    }

    /// Formats the task contract for prompt injection.
    pub fn to_prompt_string(&self) -> String {
        if self.methods.is_empty() && self.type_constraints.is_empty() && self.behavior_notes.is_empty() {
            return String::new();
        }

        let mut out = String::new();
        out.push_str("### Exact Task & Type Contract (Enforced by Harness)\n");
        if !self.methods.is_empty() {
            out.push_str("Required Signatures:\n");
            for m in &self.methods {
                let loc = m.file.as_deref().unwrap_or("target file");
                out.push_str(&format!("• In `{loc}`: `fn {}`\n", m.raw_signature));
            }
        }

        if !self.behavior_notes.is_empty() {
            out.push_str("\nBehavioral Requirements (locked):\n");
            for b in &self.behavior_notes {
                out.push_str(&format!("• {b}\n"));
            }
        }

        if !self.type_constraints.is_empty() {
            out.push_str("\nStrict Type Invariants:\n");
            for c in &self.type_constraints {
                out.push_str(&format!("• {c}\n"));
            }
        }

        out.push_str("• Operational Rule: Do NOT drift from these parameter names, placements, types, or return signatures.\n\n");
        out
    }
}

/// A sentence segment qualifies as a behavioral note when it carries a
/// requirement cue and is not a signature fragment.
fn is_behavior_note(seg: &str) -> bool {
    if seg.len() < 10 || seg.len() > 240 || !seg.contains(' ') || seg.contains('(') {
        return false;
    }
    const CUES: &[&str] = &[
        "somente", "apenas", "only", "must", "deve", "devem", "precisa",
        "não pode", "nao pode", "não deve", "nao deve", "cannot", "nunca",
        "never", "inválid", "invalid", "quando", "when ", "postcond", "precond",
    ];
    let lower = seg.to_lowercase();
    CUES.iter().any(|c| lower.contains(c))
}

/// Detects a workspace-relative `.rs` target mentioned in a task line.
fn detect_target_file(line: &str) -> Option<String> {
    let mut fallback: Option<String> = None;
    for tok in line.split_whitespace() {
        let clean = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-' && c != '\\');
        if clean.ends_with(".rs") {
            let candidate = clean.replace('\\', "/");
            if candidate.starts_with("src/") {
                return Some(candidate);
            }
            if fallback.is_none() {
                fallback = Some(candidate);
            }
        }
    }
    fallback
}

/// Extracts associated-function requirements such as
/// `User::new(name: String, age: u32) -> User` from a task line.
fn extract_associated_fn(trimmed: &str) -> Option<RequiredMethod> {
    let dc = trimmed.find("::")?;
    let owner_ok = trimmed[..dc]
        .chars()
        .last()
        .map(|c| c.is_alphanumeric() || c == '_')
        .unwrap_or(false);
    if !owner_ok {
        return None;
    }
    // Owner = the identifier run immediately before `::`.
    let owner_start = trimmed[..dc]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(dc);
    let owner = Some(trimmed[owner_start..dc].to_string());
    let after_dc = &trimmed[dc + 2..];
    let name_end = after_dc.find('(')?;
    let fn_name = after_dc[..name_end].trim().trim_matches('`');
    if fn_name.is_empty() || !fn_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let after = &after_dc[name_end..];
    let close_idx = after.find(')')?;
    if close_idx == 0 {
        return Some(RequiredMethod {
            file: detect_target_file(trimmed),
            owner,
            fn_name: fn_name.to_string(),
            raw_signature: format!("{fn_name}()"),
            self_kind: None,
            params: Vec::new(),
            return_type: None,
        });
    }
    let param_str = &after[1..close_idx];
    if param_str.contains("self") {
        return None; // handled by the method branch
    }
    let mut params = Vec::new();
    for part in param_str.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if let Some((pname, ptype)) = p.split_once(':') {
            params.push(ParamSpec {
                name: pname.trim().trim_start_matches("mut ").trim().to_string(),
                type_name: ptype.trim().to_string(),
            });
        } else {
            return None; // untyped prose, not a signature
        }
    }
    let mut raw_signature = format!("{fn_name}({param_str})");
    let ret_part = &after[close_idx + 1..];
    let return_type = match ret_part.find("->") {
        Some(idx) => {
            let tail = ret_part[idx + 2..].trim();
            let tail = tail.trim_end_matches(['.', ';', ',']);
            let clean_ret: String = tail
                .split_whitespace()
                .take_while(|s| !s.contains("que") && !s.contains("validando") && !s.ends_with('.'))
                .collect::<Vec<_>>()
                .join(" ");
            let clean_ret = clean_ret.trim_end_matches('.').trim().to_string();
            if clean_ret.is_empty() {
                None
            } else {
                raw_signature.push_str(&format!(" -> {clean_ret}"));
                Some(clean_ret)
            }
        }
        None => None,
    };

    Some(RequiredMethod {
        file: detect_target_file(trimmed),
        owner,
        fn_name: fn_name.to_string(),
        raw_signature,
        self_kind: None,
        params,
        return_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_task_contract_correctly() {
        let task = r#"
1) Em src/storage.rs adicione metodo transfer(&mut self, from_id: u64, to_id: u64, amount: f64) -> Result<(), String> que valida existencia.
2) Em src/service.rs adicione transfer_funds(&mut self, from_id: u64, to_id: u64, amount: f64) -> Result<(), String> validando amount > 0.0.
"#;
        let contract = TaskContract::extract(task);
        assert_eq!(contract.methods.len(), 2);
        assert_eq!(contract.methods[0].fn_name, "transfer");
        assert_eq!(contract.methods[0].file.as_deref(), Some("src/storage.rs"));
        assert_eq!(contract.methods[0].self_kind.as_deref(), Some("&mut self"));
        assert_eq!(contract.methods[0].params.len(), 3);
        assert_eq!(contract.methods[0].params[2].name, "amount");
        assert_eq!(contract.methods[0].params[2].type_name, "f64");

        let prompt_str = contract.to_prompt_string();
        assert!(prompt_str.contains("fn transfer(&mut self, from_id: u64, to_id: u64, amount: f64) -> Result<(), String>"));
        assert!(prompt_str.contains("`amount` must be type `f64`"));
    }

    #[test]
    fn extracts_associated_functions() {
        let task = "Em src/user.rs atualize o construtor: User::new(name: String, age: u32) -> User.";
        let contract = TaskContract::extract(task);
        assert_eq!(contract.methods.len(), 1);
        let m = &contract.methods[0];
        assert_eq!(m.fn_name, "new");
        assert_eq!(m.self_kind, None);
        assert_eq!(m.params.len(), 2);
        assert_eq!(m.params[1].type_name, "u32");
        assert_eq!(m.return_type.as_deref(), Some("User"));
    }

    #[test]
    fn captures_behavior_notes_from_sentences() {
        let task = "metodo cancel_order(&mut self, id: u64) -> Result<(), String>; Somente pedidos Pending podem ser cancelados. O pedido nunca deve ser removido do mapa.";
        let contract = TaskContract::extract(task);
        assert!(
            contract.behavior_notes.iter().any(|b| b.starts_with("Somente")),
            "{:?}",
            contract.behavior_notes
        );
        assert!(
            contract.behavior_notes.iter().any(|b| b.contains("nunca")),
            "{:?}",
            contract.behavior_notes
        );
        // Signature fragments never leak into behavior notes.
        assert!(!contract.behavior_notes.iter().any(|b| b.contains('(')));
    }

    #[test]
    fn detects_signature_drift_via_spec_gate() {
        let task = "Em src/storage.rs adicione metodo transfer(&mut self, from_id: u64, to_id: u64, amount: f64) -> Result<(), String>";
        let spec = crate::repo::spec::TaskSpec::from_task(task);

        let invalid_code = r#"
impl AccountStore {
    pub fn transfer(&mut self, from_id: u64, to_id: u64, amount: u64) -> Result<(), String> {
        Ok(())
    }
}
"#;
        let err = spec.check_patch(invalid_code, "src/storage.rs").unwrap_err();
        assert!(
            err.contains("parameter 3 (`amount`): expected type `f64`, found `u64`"),
            "{err}"
        );

        let valid_code = r#"
impl AccountStore {
    pub fn transfer(&mut self, from_id: u64, to_id: u64, amount: f64) -> Result<(), String> {
        Ok(())
    }
}
"#;
        assert!(spec.check_patch(valid_code, "src/storage.rs").is_ok());
    }
}
