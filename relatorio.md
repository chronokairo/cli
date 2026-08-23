# Relatório de Sessão — v0.9.5 Specification-Locked Execution + Bateria H Fixa

**Data:** 2026-08-23 · **Estado:** pausado pelo usuário · **Build/testes:** 360/360 ✅ zero warnings

---

## 1. Entregas da sessão

### 1.1 v0.9.5 núcleo (commit `2529bf4`, já pushed)
- `src/repo/spec.rs` — TaskSpec imutável por turno (raw task + contract + baseline de assinaturas do repo + oracle travado); gate determinístico pré-escrita (`check_patch`: Rule 1 exatidão da API exigida, Rule 2 preservação do baseline); parser de assinaturas multi-linha com genéricos.
- `src/repo/contract.rs` — extrator com `owner` (`User::new`), `self_kind`, `behavior_notes`.
- Oracle: sintetizado antes da implementação, escrito na transação, recusa `ORACLE LOCKED`.
- Flags `SPEC_LOCK` / `SPEC_ORACLE` (default on). Docs: ADR-0017, revisão honesta do bench report.

### 1.2 Bateria H fixa e versionada (este commit pendente)
```
bench/tests/
├── README.md              protocolo + estado RED intencional
├── h1-orders/             enum state machine + cancel_order -> Result<(),String>
├── h2-users/              propagação age:u32 (User::new + register)
└── h3-pool/               checkout/checkin LIFO owned-buffer
cada fixture: Cargo.toml · TASK.md (prompt verbatim) · src/ baseline ·
              tests/baseline_test.rs (verde hoje) · tests/acceptance_test.rs (RED travando a API)
bench/run_hbattery.ps1      copia p/ %TEMP%, roda `anamnesic --dir X --model M exec ... --yes`,
                            auto-build (nunca binário stale), classifica PASS / DRIFT-BLOCKED / FAIL,
                            verifica que tests/ não foi modificado; logs em bench/logs/
```
Validação estática dos fixtures: baseline 100% verde; acceptance RED com os erros exatos
(`E0599 checkout/cancel_order`, `E0061` aridade, `Cancelled` inexistente).

## 2. Bugs encontrados e corrigidos (com evidência física)

| # | Bug | Causa raiz | Correção |
|---|-----|-----------|----------|
| 1 | Runner passava `--dir` depois do subcomando | flags globais do clap | ordem `anamnesic --dir X --model M exec` |
| 2 | Extrator perdia `User::new` quando havia outro método na mesma linha | driver orientado a linha, 1 assinatura/linha | scanner unificado char-a-char sobre o texto todo (N assinaturas/linha) |
| 3 | Retorno poluído com prosa: `-> u64 deve repassar o valor ao User criado via` | stopwords PT frágeis | tokenização balanceada de tipo (`read_type_token`) — para no primeiro token fora de `<...>` |
| 4 | Redefinição EXIGIDA pela tarefa rejeitada como drift | retorno não especificado tratado como proibido | wildcard SIMÉTRICO em `matches_required` (especificado-vs-especificado continua exato — H1 `Result→Option` segue bloqueado) |
| 5 | Guarda `tests/` não disparava | binário **stale** (`cargo test` não atualiza o exe) | runner faz `cargo build` antes |
| 6 | Modelo criava testes-junk paralelos (`Buffer::new()` alucinado) | spec não era dona de tests/ | `SPECIFICATION OWNS TESTS`: contrato explícito ⇒ escrita em `tests/` recusada (com ou sem oracle) |
| 7 | Recusa de spec era FATAL mesmo com todos os testes verdes (h3 revertido verde!) | `blocked_actions` punia passo especulativo do planner | recusas de spec viram *skip* — verificação é o único juiz |
| 8 | Oracle sintetizado quebrava imports e envenenava a verificação | planner 3B alucina paths | validação por compilação: `cargo check --tests`; erros esperados (E0599/E0061/E0609/E0425 = RED intencional) vs bugs de autoria (E0432/E0433/E0583/E0428/E0616); regenera 1x com feedback do rustc; falha 2x ⇒ descarta o oracle |
| 9 | Repair one-shot morria na 1ª rodada | fluxo antigo | rodadas de repair com re-verificação entre elas + re-ask estrito |

## 3. Estado da bateria física (`qwen2.5-coder:3b`, GTX 1650)

| Task | Status | Observação |
|------|--------|-----------|
| h3-pool | ✅ **PASS** | ciclo fechou ponta a ponta: implementou, passou acceptance fixa 4/4, sem junk tests |
| h1-orders | ❌ FAIL (rollback limpo) | gate OK; semântica errada (removeu pedido em vez de marcar) — teste fixo pegou; repair não executou a correção |
| h2-users | ❌ FAIL (rollback limpo) | gate pegou tentativa de API velha 1x (correto!); modelo errou compilação nas tentativas seguintes; repair não executou |

**Gargalo ÚNICO restante:** o `qwen2.5-coder:3b` se recusa a emitir tool calls no contexto de
repair (`[repair] model would not emit tool calls`), respondendo prosa mesmo após re-ask estrito.
Tudo upstream (gate, guards, extração, oracle-validate-loop) está demonstradamente funcionando.

## 4. Próximos passos sugeridos (ordem de valor)

1. **Repair sem tool calls** — investigar `run_tool_use_iteration` (agent_loop.rs ~linha 391):
   verificar o system prompt/tools schema enviado no repair vs. plano principal; testar
   `tool_choice: required` no client Ollama; ou seed do repair com o transcript mínimo
   (última edição + diagnóstico). Hipótese: prompt gigante de feedback empurra o 3B p/ prosa.
2. **Oracle**: 3B não gera oracle compilável nem com lib.rs verbatim + hints + retry.
   Opções: (a) template determinístico por forma de assinatura; (b) `spec_oracle_model`
   configurável p/ modelo maior; (c) manter como best-effort desligado na bateria 3B.
   A bateria JÁ é robusta sem oracle (testes fixos shipped + guarda de tests/).
3. Rodadas de repair: avaliar subir `max_retries` no contexto da bateria e logar o raw reply
   do repair p/ diagnóstico definitivo do NoTools.
4. Quando bateria ≥2/3 PASS: atualizar `bench/results/e2e-*.md` + CHECKLIST.md (estado atual
   ainda diz v0.9.3) e seguir p/ tag v1.0.0 conforme release gate.

## 5. Como continuar

```powershell
cargo test                                        # 360/360 deve manter
pwsh bench/run_hbattery.ps1 -Model qwen2.5-coder:3b                    # bateria completa
pwsh bench/run_hbattery.ps1 -Model qwen2.5-coder:3b -Tasks h3-pool     # só um fixture
# logs: bench/logs/<task>-<stamp>.log · resumo JSON em bench/logs/summary-<stamp>.json
```

**Pendente de commit:** fixtures + runner + correções 2–9 desta sessão (nada commitado desde `2529bf4`).
