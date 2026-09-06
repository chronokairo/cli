use crate::error::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_gpu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    main_gpu: Option<u32>,
}

#[derive(Serialize)]
struct OllamaGenerateRequest<'a> {
    model: &'a str,
    system: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaGenerateChunk {
    response: Option<String>,
    done: Option<bool>,
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn get_cache_file() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push("chronokairo_trans_cache.json");
    p
}

fn load_cli_cache() -> HashMap<String, String> {
    let p = get_cache_file();
    if let Ok(data) = std::fs::read_to_string(&p) {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        HashMap::new()
    }
}

fn save_cli_cache(cache: &HashMap<String, String>) {
    let p = get_cache_file();
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(&p, json);
    }
}

/// Extração de texto de arquivos PDF sem dependências externas (Zero-Lib).
/// Analisa streams e blocos textuais (`BT`...`ET`, strings literais `(...)` e hex `<...>`).
pub fn extract_text_from_pdf(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Falha ao carregar PDF: {}", path.display()))?;

    if !bytes.starts_with(b"%PDF-") {
        crate::error::bail!("Arquivo informado não possui cabeçalho PDF válido.");
    }

    let mut out = String::new();
    let mut i = 0;
    let len = bytes.len();

    while i < len {
        // Look for Begin Text block 'BT'
        if i + 1 < len && bytes[i] == b'B' && bytes[i + 1] == b'T' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            i += 2;
            let mut in_parentheses = false;
            let mut in_hex = false;
            let mut escape = false;
            let mut current_literal = Vec::new();
            let mut hex_buf = Vec::new();

            while i < len {
                // Check for End Text block 'ET'
                if !in_parentheses && !in_hex && i + 1 < len && bytes[i] == b'E' && bytes[i + 1] == b'T' {
                    i += 2;
                    out.push('\n');
                    break;
                }

                let b = bytes[i];
                if in_parentheses {
                    if escape {
                        match b {
                            b'n' => current_literal.push(b'\n'),
                            b'r' => current_literal.push(b'\r'),
                            b't' => current_literal.push(b'\t'),
                            b'\\' => current_literal.push(b'\\'),
                            b'(' => current_literal.push(b'('),
                            b')' => current_literal.push(b')'),
                            _ => current_literal.push(b),
                        }
                        escape = false;
                    } else if b == b'\\' {
                        escape = true;
                    } else if b == b')' {
                        in_parentheses = false;
                        if !current_literal.is_empty() {
                            let s = String::from_utf8_lossy(&current_literal);
                            let trimmed = s.trim();
                            if !trimmed.is_empty() {
                                out.push_str(trimmed);
                                out.push(' ');
                            }
                            current_literal.clear();
                        }
                    } else {
                        current_literal.push(b);
                    }
                } else if in_hex {
                    if b == b'>' {
                        in_hex = false;
                        if let Ok(decoded) = hex_decode(&hex_buf) {
                            let s = String::from_utf8_lossy(&decoded);
                            let trimmed = s.trim();
                            if !trimmed.is_empty() {
                                out.push_str(trimmed);
                                out.push(' ');
                            }
                        }
                        hex_buf.clear();
                    } else if !b.is_ascii_whitespace() {
                        hex_buf.push(b);
                    }
                } else if b == b'(' {
                    in_parentheses = true;
                    escape = false;
                    current_literal.clear();
                } else if b == b'<' && i + 1 < len && bytes[i + 1] != b'<' {
                    in_hex = true;
                    hex_buf.clear();
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    let trimmed = out.trim();
    if trimmed.is_empty() {
        crate::error::bail!("Nenhum texto pôde ser extraído. Se for um PDF escaneado (imagem pura) ou compactado com criptografia pesada, o OCR é necessário.");
    }

    Ok(trimmed.to_string())
}

fn hex_decode(input: &[u8]) -> Result<Vec<u8>> {
    let mut res = Vec::with_capacity(input.len() / 2);
    let mut high: Option<u8> = None;
    for &b in input {
        let val = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        if let Some(h) = high {
            res.push((h << 4) | val);
            high = None;
        } else {
            high = Some(val);
        }
    }
    if let Some(h) = high {
        res.push(h << 4);
    }
    Ok(res)
}

/// Traduz o texto com streaming direto do Ollama para o terminal e salva em arquivo se solicitado
pub async fn translate_pdf_cli(
    pdf_path: &Path,
    model: &str,
    target_lang: &str,
    output_path: Option<&Path>,
    ollama_host: &str,
    gpu: Option<&str>,
) -> Result<()> {
    println!("\n  📄 Carregando: {}", pdf_path.display());
    print!("  🔍 Extraindo páginas com descompressão de streams... ");
    std::io::stdout().flush().ok();

    let raw_text = extract_text_from_pdf(pdf_path)?;
    println!("✓ Concluído ({} caracteres extraídos)\n", raw_text.len());

    // Dividir em blocos de até ~2500 caracteres
    let max_chunk_size = 2500;
    let mut chunks = Vec::new();
    let mut current_chunk = String::new();

    for paragraph in raw_text.split("\n\n") {
        if current_chunk.len() + paragraph.len() > max_chunk_size && !current_chunk.is_empty() {
            chunks.push(current_chunk.clone());
            current_chunk.clear();
        }
        current_chunk.push_str(paragraph);
        current_chunk.push_str("\n\n");
    }
    if !current_chunk.trim().is_empty() {
        chunks.push(current_chunk);
    }
    if chunks.is_empty() {
        chunks.push(raw_text);
    }

    let gpu_label = match gpu {
        Some("cpu") => "🖥️ CPU Puro (0 layers GPU)",
        Some("0") => "⚡ GPU 0 (NVIDIA GeForce GTX 1650)",
        Some("1") => "⚡ GPU 1 (Intel UHD Graphics)",
        Some(custom) => custom,
        None => "⚡ GPU Auto",
    };

    println!("  📚 Documento particionado em {} bloco(s)", chunks.len());
    println!("  🤖 Modelo Ollama: '{}' | Aceleração: {}", model, gpu_label);
    println!("  🌐 Idioma de destino: {}", target_lang);
    println!("{}\n", "─".repeat(70));

    let client = crate::http::Client::new();
    let url = format!("{}/api/generate", ollama_host.trim_end_matches('/'));
    let mut full_translation = String::new();
    let mut cache = load_cli_cache();

    let system_prompt = format!(
        "Você é um tradutor técnico e acadêmico especializado em Inteligência Artificial e Computação.\n\
        Traduza o texto fornecido diretamente para o idioma: \"{}\".\n\
        Regras:\n\
        1. Retorne APENAS o conteúdo traduzido em formato Markdown.\n\
        2. NÃO adicione introduções, explicações, cumprimentos ou notas.\n\
        3. Mantenha fórmulas, referências e formatação intactos.",
        target_lang
    );

    let ollama_opts = match gpu {
        Some("cpu") => Some(OllamaOptions {
            num_gpu: Some(0),
            main_gpu: None,
        }),
        Some("0") => Some(OllamaOptions {
            num_gpu: Some(99),
            main_gpu: Some(0),
        }),
        Some("1") => Some(OllamaOptions {
            num_gpu: Some(99),
            main_gpu: Some(1),
        }),
        Some(idx_str) if idx_str.parse::<u32>().is_ok() => Some(OllamaOptions {
            num_gpu: Some(99),
            main_gpu: Some(idx_str.parse::<u32>().unwrap()),
        }),
        _ => Some(OllamaOptions {
            num_gpu: Some(99),
            main_gpu: None,
        }),
    };

    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_hash = hash_str(chunk);
        let cache_key = format!("{}:{}:{:x}", model, target_lang, chunk_hash);

        // Check cache first
        if let Some(cached_text) = cache.get(&cache_key) {
            println!("\n  ─── [Bloco {}/{}] (Restaurado do Cache 💾) ───\n", idx + 1, chunks.len());
            print!("{}", cached_text);
            std::io::stdout().flush().ok();
            full_translation.push_str(cached_text);
            full_translation.push_str("\n\n---\n\n");
            continue;
        }

        println!("\n  ─── [Bloco {}/{}] ───\n", idx + 1, chunks.len());

        let prompt = format!(
            "Traduza o seguinte trecho para {}:\n\n\"\"\"\n{}\n\"\"\"",
            target_lang,
            chunk.trim()
        );

        let request_body = OllamaGenerateRequest {
            model,
            system: &system_prompt,
            prompt: &prompt,
            options: ollama_opts.as_ref().map(|o| OllamaOptions {
                num_gpu: o.num_gpu,
                main_gpu: o.main_gpu,
            }),
            stream: true,
        };

        let mut response = client
            .post(&url)
            .json(&request_body)
            .send()
            .await
            .with_context(|| format!("Falha ao conectar com Ollama em {}", url))?;

        if !response.status().is_success() {
            crate::error::bail!(
                "Ollama retornou erro {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }

        let mut chunk_result = String::new();

        while let Some(chunk_bytes) = response.chunk().await? {
            let text_chunk = String::from_utf8_lossy(&chunk_bytes);

            for line in text_chunk.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(parsed) = serde_json::from_str::<OllamaGenerateChunk>(line) {
                    if let Some(resp) = parsed.response {
                        print!("{}", resp);
                        std::io::stdout().flush().ok();
                        chunk_result.push_str(&resp);
                        full_translation.push_str(&resp);
                    }
                }
            }
        }

        // Save block into cache
        if !chunk_result.trim().is_empty() {
            cache.insert(cache_key, chunk_result);
            save_cli_cache(&cache);
        }

        full_translation.push_str("\n\n---\n\n");
    }

    println!("\n\n{}", "─".repeat(70));
    println!("  ✨ Tradução finalizada com sucesso!");

    if let Some(out_p) = output_path {
        let mut out_file = File::create(out_p)
            .with_context(|| format!("Falha ao criar arquivo de saída: {}", out_p.display()))?;
        out_file.write_all(full_translation.as_bytes())?;
        println!("  💾 Arquivo salvo em: {}", out_p.display());
    }

    Ok(())
}
