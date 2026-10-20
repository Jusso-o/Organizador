#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use organizador_core::{
    carregar_diario, desfazer_ultimo, eh_temporario, executar, planejar_arquivo, salvar_diario,
    AcaoPlanejada, Config, Regra,
};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Manager};

// ---------- Tipos trocados com a tela ----------

/// Regra no formato que a tela edita e que é salva em regras.json.
#[derive(Serialize, Deserialize, Clone)]
struct RegraUi {
    nome: String,
    extensoes: Vec<String>,
    destino: String,
}

/// Um arquivo da pasta, como a tela o mostra na lista de revisão.
#[derive(Serialize)]
struct Item {
    caminho: String, // caminho completo: é o que identifica o arquivo
    nome: String,
    regra: String,   // vazio se nenhuma regra combina
    destino: String, // caminho da pasta de destino sugerida (vazio se não há)
    tamanho: u64,    // bytes
    modificado: u64, // segundos desde 1970
    conflito: bool,  // já existe um arquivo com esse nome no destino
}

/// O que a pessoa decidiu para um arquivo: mover este caminho para esta pasta.
#[derive(Deserialize)]
struct Escolha {
    caminho: String,
    destino: String,
}

#[derive(Serialize)]
struct ResultadoPasta {
    caminho: String,
    criada: bool,
}

#[derive(Serialize)]
struct PreviaArquivo {
    mime: String,
    conteudo: String,
    documento: Option<Documento>,
}

#[derive(Serialize)]
struct ResultadoExclusao {
    quantidade: usize,
    erros: Vec<String>,
}

#[derive(Serialize)]
struct Documento {
    paragrafos: Vec<ParagrafoDocumento>,
}

#[derive(Serialize)]
struct ParagrafoDocumento {
    estilo: String,
    alinhamento: String,
    quebra_pagina_antes: bool,
    trechos: Vec<TrechoDocumento>,
}

#[derive(Serialize)]
struct TrechoDocumento {
    texto: String,
    negrito: bool,
    italico: bool,
    sublinhado: bool,
}

// ---------- Regras padrão, leitura e validação ----------

fn regras_padrao_lista() -> Vec<RegraUi> {
    let r = |nome: &str, exts: &[&str], destino: &str| RegraUi {
        nome: nome.to_string(),
        extensoes: exts.iter().map(|e| e.to_string()).collect(),
        destino: destino.to_string(),
    };

    vec![
        r(
            "Imagens",
            &["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg"],
            "Imagens",
        ),
        r("Vídeos", &["mp4", "mkv", "avi", "mov", "webm"], "Vídeos"),
        r("Músicas", &["mp3", "wav", "flac", "m4a", "ogg"], "Músicas"),
        r(
            "Documentos",
            &["pdf", "doc", "docx", "txt", "odt", "pptx"],
            "Documentos",
        ),
        r("Planilhas", &["xls", "xlsx", "csv", "ods"], "Planilhas"),
        r(
            "Compactados",
            &["zip", "rar", "7z", "tar", "gz"],
            "Compactados",
        ),
        r("Instaladores", &["exe", "msi"], "Instaladores"),
    ]
}

/// Um destino é só o NOME de uma pasta: nada de barras, "..", caracteres proibidos no Windows.
/// Isso impede que um destino digitado leve arquivos para fora da pasta escolhida.
fn destino_valido(destino: &str) -> bool {
    !(destino.is_empty()
        || destino == "."
        || destino == ".."
        || destino.ends_with('.')
        || destino.ends_with(' ')
        || destino.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        || destino.chars().any(|c| c.is_control()))
}

/// Limpa o que a pessoa digitou e recusa regras inválidas.
fn validar(regras: Vec<RegraUi>) -> Result<Vec<RegraUi>, String> {
    let mut saida = Vec::new();

    for r in regras {
        let nome = r.nome.trim().to_string();
        let destino = r.destino.trim().to_string();
        let extensoes: Vec<String> = r
            .extensoes
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();

        // Linha totalmente vazia: ignora em silêncio.
        if nome.is_empty() && destino.is_empty() && extensoes.is_empty() {
            continue;
        }
        if nome.is_empty() {
            return Err("Toda regra precisa de um nome.".into());
        }
        if extensoes.is_empty() {
            return Err(format!("A regra '{nome}' não tem nenhuma extensão."));
        }
        if !destino_valido(&destino) {
            return Err(format!(
                "A pasta de destino da regra '{nome}' é inválida. Use só o nome da pasta, sem barras."
            ));
        }

        saida.push(RegraUi {
            nome,
            extensoes,
            destino,
        });
    }

    Ok(saida)
}

fn pasta_dados(app: &AppHandle) -> Result<PathBuf, String> {
    let pasta = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Não achei a pasta de dados: {e}"))?;
    fs::create_dir_all(&pasta)
        .map_err(|e| format!("Não consegui criar {}: {e}", pasta.display()))?;
    Ok(pasta)
}

fn caminho_diario(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(pasta_dados(app)?.join("diario.json"))
}

fn caminho_regras(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(pasta_dados(app)?.join("regras.json"))
}

fn ler_regras(app: &AppHandle) -> Vec<RegraUi> {
    let Ok(caminho) = caminho_regras(app) else {
        return regras_padrao_lista();
    };
    match fs::read_to_string(&caminho) {
        Ok(texto) => serde_json::from_str(&texto).unwrap_or_else(|_| regras_padrao_lista()),
        Err(_) => regras_padrao_lista(),
    }
}

fn montar_config(regras: &[RegraUi]) -> Config {
    Config {
        regras: regras
            .iter()
            .map(|r| Regra {
                nome: r.nome.clone(),
                extensoes: r.extensoes.clone(),
                destino: r.destino.clone(),
            })
            .collect(),
    }
}

/// Confere a pasta escolhida e carrega as regras salvas quando solicitado.
fn preparar(app: &AppHandle, pasta: &str, usar_regras: bool) -> Result<(PathBuf, Config), String> {
    let pasta = pasta.trim();
    if pasta.is_empty() {
        return Err("Escolha uma pasta primeiro.".into());
    }
    let pasta = PathBuf::from(pasta);
    if !pasta.is_dir() {
        return Err(format!("A pasta não existe: {}", pasta.display()));
    }
    let regras = if usar_regras {
        ler_regras(app)
    } else {
        Vec::new()
    };
    Ok((pasta, montar_config(&regras)))
}

fn nome_de(caminho: &Path) -> String {
    caminho
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ---------- Comandos chamados pela tela ----------

#[tauri::command]
fn pasta_padrao(app: AppHandle) -> String {
    app.path()
        .download_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[tauri::command]
fn carregar_regras(app: AppHandle) -> Vec<RegraUi> {
    ler_regras(&app)
}

#[tauri::command]
fn regras_padrao() -> Vec<RegraUi> {
    regras_padrao_lista()
}

#[tauri::command]
fn criar_pasta(pai: String, nome: String) -> Result<ResultadoPasta, String> {
    let pai = PathBuf::from(pai.trim());
    let nome = nome.trim();
    if !pai.is_dir() {
        return Err(format!("A pasta de destino não existe: {}", pai.display()));
    }
    if !destino_valido(nome) {
        return Err("Nome de pasta inválido. Informe apenas um nome, sem barras.".into());
    }

    let nova = pai.join(nome);
    let criada = match fs::create_dir(&nova) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && nova.is_dir() => false,
        Err(e) => return Err(format!("Não consegui criar {}: {e}", nova.display())),
    };
    Ok(ResultadoPasta {
        caminho: nova.display().to_string(),
        criada,
    })
}

#[tauri::command]
fn salvar_regras(app: AppHandle, regras: Vec<RegraUi>) -> Result<Vec<RegraUi>, String> {
    let regras = validar(regras)?;
    let caminho = caminho_regras(&app)?;
    let texto = serde_json::to_string_pretty(&regras).map_err(|e| e.to_string())?;
    fs::write(&caminho, texto).map_err(|e| format!("Não consegui salvar as regras: {e}"))?;
    Ok(regras)
}

/// Lista TODOS os arquivos da pasta (não só os que têm regra), com a sugestão de destino.
/// Não move nada.
#[tauri::command]
async fn analisar(app: AppHandle, pasta: String, usar_regras: bool) -> Result<Vec<Item>, String> {
    let (pasta, config) = preparar(&app, &pasta, usar_regras)?;
    let entradas = fs::read_dir(&pasta).map_err(|e| format!("Erro ao ler a pasta: {e}"))?;

    let mut itens = Vec::new();

    for entrada in entradas.flatten() {
        let caminho = entrada.path();

        if !caminho.is_file() || eh_temporario(&caminho) {
            continue;
        }
        let Ok(meta) = fs::metadata(&caminho) else {
            continue;
        };

        let modificado = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let (regra, destino) = match planejar_arquivo(&caminho, &config) {
            Some(a) => (
                a.regra,
                a.destino
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            None => (String::new(), String::new()),
        };

        let nome = nome_de(&caminho);
        let conflito = !destino.is_empty() && Path::new(&destino).join(&nome).exists();

        itens.push(Item {
            caminho: caminho.display().to_string(),
            nome,
            regra,
            destino,
            tamanho: meta.len(),
            modificado,
            conflito,
        });
    }

    itens.sort_by(|a, b| a.nome.to_lowercase().cmp(&b.nome.to_lowercase()));
    Ok(itens)
}

/// A tela chama isto quando a pessoa troca o destino de um arquivo.
#[tauri::command]
fn verificar_conflito(nome: String, destino: String) -> bool {
    let destino = PathBuf::from(destino.trim());
    if nome.contains(['\\', '/']) || !destino.is_dir() {
        return false;
    }
    destino.join(&nome).exists()
}

fn ler_texto_xml(xml: &str) -> Result<String, String> {
    let mut leitor = Reader::from_str(xml);
    let mut texto = String::new();

    loop {
        match leitor.read_event() {
            Ok(Event::Start(elemento)) | Ok(Event::Empty(elemento)) => {
                match elemento.local_name().as_ref() {
                    "p" if !texto.is_empty() && !texto.ends_with('\n') => texto.push('\n'),
                    "tab" => texto.push('\t'),
                    "br" | "line-break" => texto.push('\n'),
                    "s" => texto.push(' '),
                    _ => {}
                }
            }
            Ok(Event::Text(evento)) => {
                texto.push_str(evento.as_ref());
            }
            Ok(Event::GeneralRef(referencia)) => {
                if let Some(caractere) = referencia
                    .resolve_char_ref()
                    .map_err(|e| format!("Referência de caractere inválida: {e}"))?
                {
                    texto.push(caractere);
                } else if let Some(entidade) =
                    quick_xml::escape::resolve_predefined_entity(referencia.as_ref())
                {
                    texto.push_str(entidade);
                } else {
                    return Err(format!(
                        "Entidade XML não suportada: &{};",
                        referencia.as_ref()
                    ));
                }
            }
            Ok(Event::CData(evento)) => {
                texto.push_str(evento.as_ref());
            }
            Ok(Event::End(elemento)) if elemento.local_name().as_ref() == "p" => {
                if !texto.ends_with('\n') {
                    texto.push('\n');
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("Documento inválido: {e}")),
        }
    }

    Ok(texto)
}

fn valor_atributo(elemento: &quick_xml::events::BytesStart<'_>, nome: &str) -> Option<String> {
    elemento.attributes().flatten().find_map(|atributo| {
        (atributo.key.local_name().as_ref() == nome)
            .then(|| {
                atributo
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .ok()
                    .map(|valor| valor.into_owned())
            })
            .flatten()
    })
}

fn propriedade_ativa(elemento: &quick_xml::events::BytesStart<'_>) -> bool {
    !matches!(
        valor_atributo(elemento, "val").as_deref(),
        Some("0" | "false" | "off" | "none")
    )
}

fn ler_documento(xml: &str) -> Result<Documento, String> {
    let mut leitor = Reader::from_str(xml);
    let mut paragrafos = Vec::new();
    let mut paragrafo: Option<ParagrafoDocumento> = None;
    let mut negrito = false;
    let mut italico = false;
    let mut sublinhado = false;

    loop {
        match leitor.read_event() {
            Ok(Event::Start(elemento)) | Ok(Event::Empty(elemento)) => {
                let nome = elemento.local_name();
                match nome.as_ref() {
                    "p" | "h" => {
                        paragrafo = Some(ParagrafoDocumento {
                            estilo: if nome.as_ref() == "h" {
                                "Heading1".into()
                            } else {
                                String::new()
                            },
                            alinhamento: String::new(),
                            quebra_pagina_antes: false,
                            trechos: Vec::new(),
                        });
                        negrito = false;
                        italico = false;
                        sublinhado = false;
                    }
                    "pStyle" => {
                        if let Some(p) = paragrafo.as_mut() {
                            p.estilo = valor_atributo(&elemento, "val").unwrap_or_default();
                        }
                    }
                    "jc" => {
                        if let Some(p) = paragrafo.as_mut() {
                            p.alinhamento = valor_atributo(&elemento, "val").unwrap_or_default();
                        }
                    }
                    "pageBreakBefore" | "lastRenderedPageBreak" => {
                        if let Some(p) = paragrafo.as_mut() {
                            p.quebra_pagina_antes = true;
                        }
                    }
                    "br" if valor_atributo(&elemento, "type").as_deref() == Some("page") => {
                        if let Some(p) = paragrafo.as_mut() {
                            p.quebra_pagina_antes = true;
                        }
                    }
                    "b" | "strong" => negrito = propriedade_ativa(&elemento),
                    "i" | "em" => italico = propriedade_ativa(&elemento),
                    "u" => sublinhado = propriedade_ativa(&elemento),
                    "tab" => {
                        if let Some(p) = paragrafo.as_mut() {
                            p.trechos.push(TrechoDocumento {
                                texto: "\t".into(),
                                negrito,
                                italico,
                                sublinhado,
                            });
                        }
                    }
                    "t" => {}
                    _ => {}
                }
            }
            Ok(Event::Text(evento)) => {
                if let Some(p) = paragrafo.as_mut() {
                    let texto = evento.as_ref();
                    if !texto.is_empty() {
                        p.trechos.push(TrechoDocumento {
                            texto: texto.to_string(),
                            negrito,
                            italico,
                            sublinhado,
                        });
                    }
                }
            }
            Ok(Event::GeneralRef(referencia)) => {
                let texto = if let Some(caractere) = referencia
                    .resolve_char_ref()
                    .map_err(|e| format!("Referência de caractere inválida: {e}"))?
                {
                    caractere.to_string()
                } else if let Some(entidade) =
                    quick_xml::escape::resolve_predefined_entity(referencia.as_ref())
                {
                    entidade.to_string()
                } else {
                    return Err(format!(
                        "Entidade XML não suportada: &{};",
                        referencia.as_ref()
                    ));
                };
                if let Some(p) = paragrafo.as_mut() {
                    p.trechos.push(TrechoDocumento {
                        texto,
                        negrito,
                        italico,
                        sublinhado,
                    });
                }
            }
            Ok(Event::End(elemento)) => match elemento.local_name().as_ref() {
                "p" | "h" => {
                    if let Some(p) = paragrafo.take() {
                        paragrafos.push(p);
                    }
                }
                "b" | "strong" => negrito = false,
                "i" | "em" => italico = false,
                "u" => sublinhado = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("Documento inválido: {e}")),
        }
    }

    Ok(Documento { paragrafos })
}

fn ler_documento_comprimido<R: Read + Seek>(
    arquivo: R,
    entrada: &str,
    limite: u64,
) -> Result<Vec<u8>, String> {
    let mut arquivo_zip =
        zip::ZipArchive::new(arquivo).map_err(|e| format!("Documento inválido: {e}"))?;
    let xml = arquivo_zip
        .by_name(entrada)
        .map_err(|e| format!("Não encontrei o conteúdo do documento: {e}"))?;
    if xml.size() > limite {
        return Err("O documento excede o limite de 20 MB para leitura.".into());
    }

    let mut bytes = Vec::with_capacity(xml.size() as usize);
    xml.take(limite + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Não consegui ler o documento: {e}"))?;
    if bytes.len() as u64 > limite {
        return Err("O documento excede o limite de 20 MB para leitura.".into());
    }

    let xml = std::str::from_utf8(&bytes)
        .map_err(|_| "O documento contém texto em uma codificação não suportada.".to_string())?;
    Ok(ler_texto_xml(xml)?.into_bytes())
}

fn ler_documento_comprimido_com_formatacao<R: Read + Seek>(
    arquivo: R,
    entrada: &str,
    limite: u64,
) -> Result<Documento, String> {
    let mut arquivo_zip =
        zip::ZipArchive::new(arquivo).map_err(|e| format!("Documento inválido: {e}"))?;
    let xml = arquivo_zip
        .by_name(entrada)
        .map_err(|e| format!("Não encontrei o conteúdo do documento: {e}"))?;
    if xml.size() > limite {
        return Err("O documento excede o limite de 20 MB para leitura.".into());
    }
    let mut bytes = Vec::with_capacity(xml.size() as usize);
    xml.take(limite + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Não consegui ler o documento: {e}"))?;
    if bytes.len() as u64 > limite {
        return Err("O documento excede o limite de 20 MB para leitura.".into());
    }
    let xml = std::str::from_utf8(&bytes)
        .map_err(|_| "O documento contém texto em uma codificação não suportada.".to_string())?;
    ler_documento(xml)
}

#[tauri::command]
fn visualizar_arquivo(pasta: String, caminho: String) -> Result<PreviaArquivo, String> {
    const LIMITE_BYTES: u64 = 20 * 1024 * 1024;

    let pasta = PathBuf::from(pasta.trim())
        .canonicalize()
        .map_err(|e| format!("Não consegui acessar a pasta selecionada: {e}"))?;
    let caminho = PathBuf::from(caminho.trim())
        .canonicalize()
        .map_err(|e| format!("Não consegui acessar o arquivo: {e}"))?;

    if caminho.parent() != Some(pasta.as_path()) || !caminho.is_file() {
        return Err("O arquivo não pertence à pasta selecionada.".into());
    }
    if eh_temporario(&caminho) {
        return Err("Este arquivo temporário não pode ser pré-visualizado.".into());
    }

    let extensao = caminho
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime = match extensao.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "docx" | "odt" => "application/x-organizador-document",
        "txt" | "md" | "csv" | "json" | "log" | "toml" | "yaml" | "yml" => "text/plain",
        _ => return Err("Não há pré-visualização disponível para este tipo de arquivo.".into()),
    };

    let metadados =
        fs::metadata(&caminho).map_err(|e| format!("Não consegui ler os dados do arquivo: {e}"))?;
    if metadados.len() > LIMITE_BYTES {
        return Err("O arquivo excede o limite de 20 MB para pré-visualização.".into());
    }

    let documento = if extensao == "docx" || extensao == "odt" {
        let arquivo =
            fs::File::open(&caminho).map_err(|e| format!("Não consegui abrir o arquivo: {e}"))?;
        let entrada = if extensao == "docx" {
            "word/document.xml"
        } else {
            "content.xml"
        };
        Some(ler_documento_comprimido_com_formatacao(
            arquivo,
            entrada,
            LIMITE_BYTES,
        )?)
    } else {
        None
    };

    let bytes = if documento.is_some() {
        Vec::new()
    } else {
        let arquivo =
            fs::File::open(&caminho).map_err(|e| format!("Não consegui abrir o arquivo: {e}"))?;
        let mut bytes = Vec::with_capacity(metadados.len() as usize);
        arquivo
            .take(LIMITE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Não consegui ler o arquivo: {e}"))?;
        bytes
    };
    if bytes.len() as u64 > LIMITE_BYTES {
        return Err("O arquivo excede o limite de 20 MB para pré-visualização.".into());
    }
    if mime == "text/plain" {
        std::str::from_utf8(&bytes)
            .map_err(|_| "Este arquivo de texto não está codificado em UTF-8.".to_string())?;
    }

    Ok(PreviaArquivo {
        mime: mime.to_string(),
        conteudo: STANDARD.encode(bytes),
        documento,
    })
}

#[tauri::command]
fn arquivar_arquivos(
    pasta: &Path,
    lixeira: &Path,
    caminhos: Vec<String>,
) -> Result<(organizador_core::Lote, Vec<String>), String> {
    if caminhos.is_empty() {
        return Err("Selecione pelo menos um arquivo para excluir.".into());
    }

    let mut erros = Vec::new();
    let mut plano = Vec::new();
    let mut unicos = std::collections::HashSet::new();
    for caminho in caminhos {
        let caminho = PathBuf::from(caminho.trim());
        if !unicos.insert(caminho.clone()) {
            continue;
        }

        match fs::symlink_metadata(&caminho) {
            Ok(metadados) if metadados.file_type().is_file() => {}
            Ok(_) => {
                erros.push(format!("{}: não é um arquivo regular", nome_de(&caminho)));
                continue;
            }
            Err(e) => {
                erros.push(format!("{}: {e}", nome_de(&caminho)));
                continue;
            }
        }

        let caminho_canonico = match caminho.canonicalize() {
            Ok(caminho) => caminho,
            Err(e) => {
                erros.push(format!("{}: {e}", nome_de(&caminho)));
                continue;
            }
        };
        if caminho_canonico.parent() != Some(pasta) || eh_temporario(&caminho_canonico) {
            erros.push(format!(
                "{}: o arquivo não pertence à pasta de origem",
                nome_de(&caminho)
            ));
            continue;
        }

        plano.push(AcaoPlanejada {
            origem: caminho_canonico,
            destino: lixeira.join(nome_de(&caminho)),
            regra: "Lixeira do Organizador".into(),
        });
    }

    if plano.is_empty() {
        return Err(format!(
            "Não consegui mover arquivos para a lixeira: {}",
            erros.join("; ")
        ));
    }

    let (lote, mut erros_execucao) = executar(&plano);
    erros.append(&mut erros_execucao);
    Ok((lote, erros))
}

fn restaurar_movimentos(lote: &organizador_core::Lote) -> Vec<String> {
    let mut erros = Vec::new();
    for movimento in lote.movimentos.iter().rev() {
        if movimento.origem.exists() {
            erros.push(format!(
                "{}: o caminho original já existe",
                movimento.origem.display()
            ));
            continue;
        }
        if let Err(erro) = fs::rename(&movimento.destino, &movimento.origem) {
            match fs::copy(&movimento.destino, &movimento.origem) {
                Ok(_) => {
                    if let Err(erro_remocao) = fs::remove_file(&movimento.destino) {
                        let _ = fs::remove_file(&movimento.origem);
                        erros.push(format!(
                            "{}: não consegui remover a cópia da lixeira após restaurar: {erro_remocao}",
                            movimento.destino.display()
                        ));
                    }
                }
                Err(erro_copia) => erros.push(format!(
                    "{}: não consegui restaurar após falha no histórico ({erro}; {erro_copia})",
                    movimento.origem.display()
                )),
            }
        }
    }
    erros
}

#[tauri::command]
fn excluir_arquivos(
    app: AppHandle,
    pasta: String,
    caminhos: Vec<String>,
) -> Result<ResultadoExclusao, String> {
    let pasta = PathBuf::from(pasta.trim())
        .canonicalize()
        .map_err(|e| format!("Não consegui acessar a pasta selecionada: {e}"))?;
    if !pasta.is_dir() {
        return Err("A pasta de origem não existe.".into());
    }

    let diario_path = caminho_diario(&app)?;
    let mut diario = carregar_diario(&diario_path)
        .map_err(|e| format!("Erro ao ler o diário, nada foi enviado à lixeira: {e}"))?;
    let raiz_lixeira = pasta_dados(&app)?.join("Lixeira");
    fs::create_dir_all(&raiz_lixeira)
        .map_err(|e| format!("Não consegui criar a lixeira do Organizador: {e}"))?;
    let lote_lixeira = raiz_lixeira.join(format!(
        "exclusao-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("Não consegui gerar um identificador para a exclusão: {e}"))?
            .as_nanos()
    ));
    fs::create_dir(&lote_lixeira)
        .map_err(|e| format!("Não consegui preparar a lixeira do Organizador: {e}"))?;

    let (lote, erros) = match arquivar_arquivos(&pasta, &lote_lixeira, caminhos) {
        Ok(resultado) => resultado,
        Err(e) => {
            let _ = fs::remove_dir(&lote_lixeira);
            return Err(e);
        }
    };
    let quantidade = lote.movimentos.len();
    if quantidade == 0 {
        let _ = fs::remove_dir(&lote_lixeira);
        return Err(format!(
            "Não consegui mover arquivos para a lixeira: {}",
            erros.join("; ")
        ));
    }

    diario.push(lote);
    if let Err(erro) = salvar_diario(&diario_path, &diario) {
        let lote = diario.pop().expect("o lote acabou de ser inserido");
        let erros_restauracao = restaurar_movimentos(&lote);
        let _ = fs::remove_dir(&lote_lixeira);
        return if erros_restauracao.is_empty() {
            Err(format!(
                "Não consegui salvar o histórico de desfazer; os {quantidade} arquivo(s) foram restaurados: {erro}"
            ))
        } else {
            Err(format!(
                "Não consegui salvar o histórico de desfazer ({erro}) nem restaurar todos os arquivos: {}",
                erros_restauracao.join("; ")
            ))
        };
    }

    Ok(ResultadoExclusao { quantidade, erros })
}

/// Move só o que a pessoa marcou, para a pasta que ela escolheu para cada arquivo.
#[tauri::command]
async fn organizar_selecionados(
    app: AppHandle,
    pasta: String,
    escolhas: Vec<Escolha>,
) -> Result<String, String> {
    let pasta = PathBuf::from(pasta.trim());
    if !pasta.is_dir() {
        return Err(format!("A pasta não existe: {}", pasta.display()));
    }
    if escolhas.is_empty() {
        return Ok("Nenhum arquivo selecionado.".into());
    }

    let mut plano = Vec::new();
    let mut avisos = Vec::new();

    // Nunca confiamos cegamente no que vem da tela: conferimos cada pedido.
    for e in escolhas {
        let origem = PathBuf::from(&e.caminho);
        let destino_texto = e.destino.trim();
        let destino = PathBuf::from(destino_texto);
        let nome = nome_de(&origem);

        if origem.parent() != Some(pasta.as_path()) || !origem.is_file() {
            avisos.push(format!("{nome}: arquivo não encontrado nesta pasta"));
            continue;
        }
        let destino_de_regra = destino.parent() == Some(pasta.as_path())
            && destino
                .file_name()
                .and_then(|nome| nome.to_str())
                .is_some_and(destino_valido);
        if destino_texto.is_empty() || (!destino.is_dir() && !destino_de_regra) {
            avisos.push(format!("{nome}: pasta de destino não encontrada"));
            continue;
        }

        plano.push(AcaoPlanejada {
            destino: destino.join(&nome),
            origem,
            regra: destino.display().to_string(),
        });
    }

    if plano.is_empty() {
        return Err(avisos.join("; "));
    }

    // Lê o diário ANTES de mover: se estiver corrompido, paramos sem tocar nos arquivos.
    let caminho = caminho_diario(&app)?;
    let mut diario = carregar_diario(&caminho)
        .map_err(|e| format!("Erro ao ler o diário, nada foi movido: {e}"))?;

    let (lote, erros) = executar(&plano);
    let movidos = lote.movimentos.len();

    let mut texto = format!("{movidos} arquivo(s) movido(s).");
    for a in avisos.iter().chain(erros.iter()) {
        texto.push_str(&format!(" Aviso: {a}."));
    }

    if movidos > 0 {
        diario.push(lote);
        salvar_diario(&caminho, &diario).map_err(|e| {
            format!("Os arquivos foram movidos, mas não consegui gravar o diário (não será possível desfazer): {e}")
        })?;
    }

    Ok(texto)
}

#[tauri::command]
async fn desfazer(app: AppHandle) -> Result<String, String> {
    let diario = caminho_diario(&app)?;
    let (n, avisos) = desfazer_ultimo(&diario).map_err(|e| format!("Erro ao desfazer: {e}"))?;

    if n == 0 && avisos.is_empty() {
        return Ok("Nada para desfazer.".into());
    }

    let mut texto = format!("{n} arquivo(s) voltaram para onde estavam.");
    for a in avisos {
        texto.push_str(&format!(" Aviso: {a}."));
    }
    Ok(texto)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            pasta_padrao,
            carregar_regras,
            regras_padrao,
            criar_pasta,
            salvar_regras,
            analisar,
            verificar_conflito,
            visualizar_arquivo,
            excluir_arquivos,
            organizar_selecionados,
            desfazer
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o aplicativo");
}

#[cfg(test)]
mod tests {
    use super::{
        arquivar_arquivos, ler_documento_comprimido, ler_documento_comprimido_com_formatacao,
        ler_texto_xml, restaurar_movimentos,
    };
    use organizador_core::{desfazer_ultimo, salvar_diario};
    use std::fs;
    use std::io::{Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;

    #[test]
    fn extrai_texto_de_paragrafos_word_e_entidades_xml() {
        let xml = r#"<w:document><w:p><w:r><w:t>Olá &amp; mundo</w:t></w:r></w:p><w:p><w:r><w:tab/><w:t>Segundo</w:t></w:r></w:p></w:document>"#;
        assert_eq!(ler_texto_xml(xml).unwrap(), "Olá & mundo\n\tSegundo\n");
    }

    #[test]
    fn le_conteudo_docx_de_arquivo_zip() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"<w:document><w:p><w:r><w:t>Texto de teste</w:t></w:r></w:p></w:document>")
            .unwrap();
        let dados = zip.finish().unwrap().into_inner();

        let texto =
            ler_documento_comprimido(Cursor::new(dados), "word/document.xml", 1024).unwrap();
        assert_eq!(String::from_utf8(texto).unwrap(), "Texto de teste\n");
    }

    #[test]
    fn le_conteudo_odt_de_arquivo_zip() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("content.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"<office:document><text:p>Texto ODT</text:p></office:document>")
            .unwrap();
        let dados = zip.finish().unwrap().into_inner();

        let texto = ler_documento_comprimido(Cursor::new(dados), "content.xml", 1024).unwrap();
        assert_eq!(String::from_utf8(texto).unwrap(), "Texto ODT\n");
    }

    #[test]
    fn preserva_formatacao_basica_dos_paragrafos_docx() {
        let xml = r#"<w:document><w:p><w:pPr><w:pStyle w:val="Title"/><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Título</w:t></w:r></w:p></w:document>"#;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        let dados = zip.finish().unwrap().into_inner();

        let documento =
            ler_documento_comprimido_com_formatacao(Cursor::new(dados), "word/document.xml", 1024)
                .unwrap();
        assert_eq!(documento.paragrafos[0].estilo, "Title");
        assert_eq!(documento.paragrafos[0].alinhamento, "center");
        assert!(documento.paragrafos[0].trechos[0].negrito);
    }

    #[test]
    fn envia_arquivo_para_lixeira_e_desfazer_o_restaura() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pasta = std::env::temp_dir().join(format!(
            "organizador-lixeira-{}-{nonce}",
            std::process::id()
        ));
        let externa = std::env::temp_dir().join(format!(
            "organizador-lixeira-externa-{}-{nonce}.txt",
            std::process::id()
        ));
        let lixeira = pasta.join("lixeira");
        let diario = pasta.join("diario.json");
        fs::create_dir(&pasta).unwrap();
        fs::create_dir(&lixeira).unwrap();
        let dentro = pasta.join("teste.txt");
        fs::write(&dentro, "teste").unwrap();
        fs::write(&externa, "preservado").unwrap();

        let (lote, erros) = arquivar_arquivos(
            &fs::canonicalize(&pasta).unwrap(),
            &lixeira,
            vec![dentro.display().to_string(), externa.display().to_string()],
        )
        .unwrap();
        assert_eq!(lote.movimentos.len(), 1);
        assert_eq!(erros.len(), 1);
        assert!(!dentro.exists());
        assert_eq!(fs::read_to_string(&externa).unwrap(), "preservado");

        salvar_diario(&diario, &[lote]).unwrap();
        let (restaurados, avisos) = desfazer_ultimo(&diario).unwrap();
        assert_eq!(restaurados, 1);
        assert!(avisos.is_empty());
        assert_eq!(fs::read_to_string(&dentro).unwrap(), "teste");

        fs::remove_file(externa).unwrap();
        fs::remove_file(dentro).unwrap();
        fs::remove_file(diario).unwrap();
        if lixeira.exists() {
            fs::remove_dir(lixeira).unwrap();
        }
        fs::remove_dir(pasta).unwrap();
    }

    #[test]
    fn restaura_movimentos_quando_historico_nao_pode_ser_salvo() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pasta = std::env::temp_dir().join(format!(
            "organizador-rollback-{}-{nonce}",
            std::process::id()
        ));
        let lixeira = pasta.join("lixeira");
        fs::create_dir_all(&lixeira).unwrap();
        let origem = pasta.join("restaurar.txt");
        let destino = lixeira.join("restaurar.txt");
        fs::write(&destino, "conteúdo").unwrap();

        let lote = organizador_core::Lote {
            id: nonce as u64,
            movimentos: vec![organizador_core::Movimento {
                origem: origem.clone(),
                destino: destino.clone(),
            }],
        };
        assert!(restaurar_movimentos(&lote).is_empty());
        assert_eq!(fs::read_to_string(&origem).unwrap(), "conteúdo");
        assert!(!destino.exists());

        fs::remove_file(origem).unwrap();
        fs::remove_dir(lixeira).unwrap();
        fs::remove_dir(pasta).unwrap();
    }
}
