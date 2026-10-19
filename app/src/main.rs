#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use organizador_core::{
    carregar_diario, desfazer_ultimo, executar, monitorar, planejar, salvar_diario, Config, Regra,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State};

/// Estado compartilhado: a bandeira de parada do vigia, se ele estiver rodando.
#[derive(Default)]
struct Vigia {
    parar: Mutex<Option<Arc<AtomicBool>>>,
}

/// Regra no formato que a tela edita e que é salva em regras.json.
#[derive(Serialize, Deserialize, Clone)]
struct RegraUi {
    nome: String,
    extensoes: Vec<String>,
    destino: String,
}

#[derive(Serialize)]
struct LinhaPlano {
    regra: String,
    origem: String,
    destino: String,
}

// ---------- Regras padrão, leitura e validação ----------

fn regras_padrao_lista() -> Vec<RegraUi> {
    let r = |nome: &str, exts: &[&str], destino: &str| RegraUi {
        nome: nome.to_string(),
        extensoes: exts.iter().map(|e| e.to_string()).collect(),
        destino: destino.to_string(),
    };

    vec![
        r("Imagens", &["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg"], "Imagens"),
        r("Vídeos", &["mp4", "mkv", "avi", "mov", "webm"], "Vídeos"),
        r("Músicas", &["mp3", "wav", "flac", "m4a", "ogg"], "Músicas"),
        r("Documentos", &["pdf", "doc", "docx", "txt", "odt", "pptx"], "Documentos"),
        r("Planilhas", &["xls", "xlsx", "csv", "ods"], "Planilhas"),
        r("Compactados", &["zip", "rar", "7z", "tar", "gz"], "Compactados"),
        r("Instaladores", &["exe", "msi"], "Instaladores"),
    ]
}

/// Limpa o que a pessoa digitou e recusa regras que poderiam mover arquivos para fora da pasta.
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
        if destino.is_empty()
            || destino == "."
            || destino == ".."
            || destino.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        {
            return Err(format!(
                "A pasta de destino da regra '{nome}' é inválida. Use só o nome da pasta, sem barras."
            ));
        }

        saida.push(RegraUi { nome, extensoes, destino });
    }

    Ok(saida)
}

fn pasta_dados(app: &AppHandle) -> Result<PathBuf, String> {
    let pasta = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Não achei a pasta de dados: {e}"))?;
    std::fs::create_dir_all(&pasta)
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
    match std::fs::read_to_string(&caminho) {
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

/// Confere a pasta escolhida e carrega as regras salvas.
fn preparar(app: &AppHandle, pasta: &str) -> Result<(PathBuf, Config), String> {
    let pasta = pasta.trim();
    if pasta.is_empty() {
        return Err("Escolha uma pasta primeiro.".into());
    }
    let pasta = PathBuf::from(pasta);
    if !pasta.is_dir() {
        return Err(format!("A pasta não existe: {}", pasta.display()));
    }
    Ok((pasta, montar_config(&ler_regras(app))))
}

fn nome_de(caminho: &Path) -> String {
    caminho
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Transforma "C:\...\foto.png -> C:\...\Imagens\foto.png" em "foto.png → Imagens".
fn resumir(msg: &str) -> String {
    match msg.split_once(" -> ") {
        Some((origem, destino)) => {
            let arquivo = nome_de(Path::new(origem));
            let pasta = Path::new(destino).parent().map(nome_de).unwrap_or_default();
            format!("{arquivo} → {pasta}")
        }
        None => msg.to_string(),
    }
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
fn salvar_regras(app: AppHandle, regras: Vec<RegraUi>) -> Result<Vec<RegraUi>, String> {
    let regras = validar(regras)?;
    let caminho = caminho_regras(&app)?;
    let texto = serde_json::to_string_pretty(&regras).map_err(|e| e.to_string())?;
    std::fs::write(&caminho, texto).map_err(|e| format!("Não consegui salvar as regras: {e}"))?;
    Ok(regras)
}

#[tauri::command]
async fn simular(app: AppHandle, pasta: String) -> Result<Vec<LinhaPlano>, String> {
    let (pasta, config) = preparar(&app, &pasta)?;
    let plano = planejar(&pasta, &config).map_err(|e| format!("Erro ao ler a pasta: {e}"))?;

    Ok(plano
        .into_iter()
        .map(|a| LinhaPlano {
            regra: a.regra,
            origem: nome_de(&a.origem),
            destino: a.destino.parent().map(nome_de).unwrap_or_default(),
        })
        .collect())
}

#[tauri::command]
async fn organizar_agora(
    app: AppHandle,
    estado: State<'_, Vigia>,
    pasta: String,
) -> Result<String, String> {
    if estado.parar.lock().unwrap().is_some() {
        return Err("Desligue a organização automática antes de organizar manualmente.".into());
    }

    let (pasta, config) = preparar(&app, &pasta)?;
    let plano = planejar(&pasta, &config).map_err(|e| format!("Erro ao ler a pasta: {e}"))?;

    if plano.is_empty() {
        return Ok("Nada para organizar.".into());
    }

    // Lê o diário ANTES de mover: se estiver corrompido, paramos sem tocar nos arquivos.
    let caminho = caminho_diario(&app)?;
    let mut diario = carregar_diario(&caminho)
        .map_err(|e| format!("Erro ao ler o diário, nada foi movido: {e}"))?;

    let (lote, erros) = executar(&plano);
    let movidos = lote.movimentos.len();

    let mut texto = format!("{movidos} arquivo(s) organizado(s).");
    for e in erros {
        texto.push_str(&format!(" Aviso: {e}."));
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
fn iniciar_vigia(app: AppHandle, estado: State<'_, Vigia>, pasta: String) -> Result<(), String> {
    let mut atual = estado.parar.lock().unwrap();
    if atual.is_some() {
        return Err("A organização automática já está ligada.".into());
    }

    let (pasta, config) = preparar(&app, &pasta)?;
    let diario = caminho_diario(&app)?;
    let parar = Arc::new(AtomicBool::new(false));
    *atual = Some(parar.clone());

    // O vigia roda numa thread própria, para não travar a janela.
    thread::spawn(move || {
        let app_log = app.clone();
        let resultado = monitorar(&pasta, &config, &diario, &parar, move |msg| {
            let _ = app_log.emit("log", resumir(msg));
        });

        if let Err(e) = resultado {
            let _ = app.emit("log", format!("Erro no monitoramento: {e}"));
        }

        *app.state::<Vigia>().parar.lock().unwrap() = None;
        let _ = app.emit("vigia-parou", ());
    });

    Ok(())
}

#[tauri::command]
fn parar_vigia(estado: State<'_, Vigia>) {
    if let Some(bandeira) = estado.parar.lock().unwrap().as_ref() {
        bandeira.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
async fn desfazer(app: AppHandle, estado: State<'_, Vigia>) -> Result<String, String> {
    if estado.parar.lock().unwrap().is_some() {
        return Err("Desligue a organização automática antes de desfazer.".into());
    }

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
        .manage(Vigia::default())
        .invoke_handler(tauri::generate_handler![
            pasta_padrao,
            carregar_regras,
            regras_padrao,
            salvar_regras,
            simular,
            organizar_agora,
            iniciar_vigia,
            parar_vigia,
            desfazer
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o aplicativo");
}