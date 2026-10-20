#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use organizador_core::{
    carregar_diario, desfazer_ultimo, eh_temporario, executar, planejar_arquivo, salvar_diario,
    AcaoPlanejada, Config, Regra,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
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
        "txt" | "md" | "csv" | "json" | "log" | "toml" | "yaml" | "yml" => "text/plain",
        _ => return Err("Não há pré-visualização disponível para este tipo de arquivo.".into()),
    };

    let metadados =
        fs::metadata(&caminho).map_err(|e| format!("Não consegui ler os dados do arquivo: {e}"))?;
    if metadados.len() > LIMITE_BYTES {
        return Err("O arquivo excede o limite de 20 MB para pré-visualização.".into());
    }

    let arquivo =
        fs::File::open(&caminho).map_err(|e| format!("Não consegui abrir o arquivo: {e}"))?;
    let mut bytes = Vec::with_capacity(metadados.len() as usize);
    arquivo
        .take(LIMITE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Não consegui ler o arquivo: {e}"))?;
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
    })
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
            organizar_selecionados,
            desfazer
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o aplicativo");
}
