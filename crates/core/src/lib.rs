use notify::{EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------- Regras ----------

#[derive(Debug, Deserialize)]
pub struct Regra {
    pub nome: String,
    pub extensoes: Vec<String>,
    pub destino: String,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub regras: Vec<Regra>,
}

#[derive(Debug)]
pub struct AcaoPlanejada {
    pub origem: PathBuf,
    pub destino: PathBuf,
    pub regra: String,
}

// ---------- Diário ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct Movimento {
    pub origem: PathBuf,
    pub destino: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Lote {
    pub id: u64,
    pub movimentos: Vec<Movimento>,
}

// ---------- Leitura de regras e planejamento ----------

pub fn carregar_config(caminho: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let texto = fs::read_to_string(caminho)?;
    let config: Config = toml::from_str(&texto)?;
    Ok(config)
}

const TEMPORARIAS: &[&str] = &[
    "crdownload", "part", "tmp", "temp", "download", "partial", "opdownload",
];

/// Arquivos que não devem ser tocados: downloads incompletos, travas do Office, ocultos.
pub fn eh_temporario(caminho: &Path) -> bool {
    let nome = caminho
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if nome.starts_with("~$") || nome.starts_with('.') || nome == "desktop.ini" {
        return true;
    }

    let ext = caminho
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    TEMPORARIAS.contains(&ext.as_str())
}

/// Decide o que fazer com UM arquivo. Devolve None se não há o que fazer.
pub fn planejar_arquivo(caminho: &Path, config: &Config) -> Option<AcaoPlanejada> {
    if !caminho.is_file() || eh_temporario(caminho) {
        return None;
    }

    let ext = caminho.extension()?.to_str()?.to_lowercase();
    let pasta = caminho.parent()?;

    let regra = config
        .regras
        .iter()
        .find(|r| r.extensoes.iter().any(|e| e.to_lowercase() == ext))?;

    let destino = pasta.join(&regra.destino).join(caminho.file_name()?);

    Some(AcaoPlanejada {
        origem: caminho.to_path_buf(),
        destino,
        regra: regra.nome.clone(),
    })
}

/// Planeja a pasta inteira (usado pela simulação e pelo 'executar').
pub fn planejar(pasta: &Path, config: &Config) -> io::Result<Vec<AcaoPlanejada>> {
    let mut plano = Vec::new();

    for entrada in fs::read_dir(pasta)? {
        let caminho = entrada?.path();
        if let Some(acao) = planejar_arquivo(&caminho, config) {
            plano.push(acao);
        }
    }

    Ok(plano)
}

// ---------- Execução ----------

fn caminho_livre(desejado: PathBuf) -> PathBuf {
    if !desejado.exists() {
        return desejado;
    }

    let pasta = desejado.parent().unwrap().to_path_buf();
    let nome = desejado.file_stem().unwrap().to_string_lossy().to_string();
    let ext = desejado.extension().map(|e| e.to_string_lossy().to_string());

    let mut n = 1;
    loop {
        let novo_nome = match &ext {
            Some(e) => format!("{nome} ({n}).{e}"),
            None => format!("{nome} ({n})"),
        };
        let candidato = pasta.join(novo_nome);
        if !candidato.exists() {
            return candidato;
        }
        n += 1;
    }
}

fn mover_arquivo(origem: &Path, destino: &Path) -> io::Result<()> {
    if let Some(pai) = destino.parent() {
        fs::create_dir_all(pai)?;
    }

    match fs::rename(origem, destino) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(origem, destino)?;
            if let Err(e) = fs::remove_file(origem) {
                let _ = fs::remove_file(destino);
                return Err(e);
            }
            Ok(())
        }
    }
}

pub fn executar(plano: &[AcaoPlanejada]) -> (Lote, Vec<String>) {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut movimentos = Vec::new();
    let mut erros = Vec::new();

    for acao in plano {
        let destino = caminho_livre(acao.destino.clone());

        match mover_arquivo(&acao.origem, &destino) {
            Ok(()) => movimentos.push(Movimento {
                origem: acao.origem.clone(),
                destino,
            }),
            Err(e) => erros.push(format!("{}: {e}", acao.origem.display())),
        }
    }

    (Lote { id, movimentos }, erros)
}

// ---------- Diário e desfazer ----------

pub fn carregar_diario(caminho: &Path) -> io::Result<Vec<Lote>> {
    if !caminho.exists() {
        return Ok(Vec::new());
    }
    let texto = fs::read_to_string(caminho)?;
    Ok(serde_json::from_str(&texto)?)
}

pub fn salvar_diario(caminho: &Path, diario: &[Lote]) -> io::Result<()> {
    let texto = serde_json::to_string_pretty(diario)?;
    fs::write(caminho, texto)
}

pub fn desfazer_ultimo(caminho: &Path) -> io::Result<(usize, Vec<String>)> {
    let mut diario = carregar_diario(caminho)?;

    let Some(lote) = diario.pop() else {
        return Ok((0, Vec::new()));
    };

    let mut restaurados = 0;
    let mut avisos = Vec::new();

    for mov in lote.movimentos.iter().rev() {
        if !mov.destino.exists() {
            avisos.push(format!("não encontrado: {}", mov.destino.display()));
            continue;
        }

        let volta = caminho_livre(mov.origem.clone());

        match mover_arquivo(&mov.destino, &volta) {
            Ok(()) => {
                restaurados += 1;
                if let Some(pasta) = mov.destino.parent() {
                    let _ = fs::remove_dir(pasta);
                }
            }
            Err(e) => avisos.push(format!("{}: {e}", mov.destino.display())),
        }
    }

    salvar_diario(caminho, &diario)?;
    Ok((restaurados, avisos))
}

// ---------- Monitoramento em tempo real ----------

/// Quanto tempo sem novos eventos antes de considerar mover um arquivo.
const ESPERA: Duration = Duration::from_secs(2);

/// Um arquivo que "mexeu" e está aguardando estabilizar.
struct Pendente {
    ultimo_evento: Instant,
    tamanho_anterior: Option<u64>,
}

/// Se outro programa ainda está escrevendo, abrir para escrita costuma falhar no Windows.
/// Arquivo somente leitura (PermissionDenied) não conta como "em uso".
fn pode_escrever(caminho: &Path) -> bool {
    match OpenOptions::new().write(true).open(caminho) {
        Ok(_) => true,
        Err(e) => e.kind() == io::ErrorKind::PermissionDenied,
    }
}

/// Vigia a pasta para sempre, organizando arquivos novos quando estabilizam.
/// `log` recebe as mensagens para quem chamou decidir onde mostrar.
pub fn monitorar(
    pasta: &Path,
    config: &Config,
    diario: &Path,
    mut log: impl FnMut(&str),
) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(pasta, RecursiveMode::NonRecursive)?;

    let mut pendentes: HashMap<PathBuf, Pendente> = HashMap::new();

    loop {
        // 1) Espera até 500 ms por um evento do sistema de arquivos.
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(evento)) => {
                if matches!(evento.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    for caminho in evento.paths {
                        pendentes.insert(
                            caminho,
                            Pendente {
                                ultimo_evento: Instant::now(),
                                tamanho_anterior: None,
                            },
                        );
                    }
                }
            }
            Ok(Err(e)) => log(&format!("Erro do monitor: {e}")),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // 2) Quais pendentes já estabilizaram?
        let mut prontos = Vec::new();
        let mut descartar = Vec::new();

        for (caminho, p) in pendentes.iter_mut() {
            if p.ultimo_evento.elapsed() < ESPERA {
                continue;
            }

            if !caminho.is_file() || eh_temporario(caminho) {
                descartar.push(caminho.clone());
                continue;
            }

            let Ok(meta) = fs::metadata(caminho) else {
                descartar.push(caminho.clone());
                continue;
            };
            let tamanho = meta.len();

            if p.tamanho_anterior == Some(tamanho) && pode_escrever(caminho) {
                prontos.push(caminho.clone());
            } else {
                p.tamanho_anterior = Some(tamanho); // confere de novo na próxima volta
            }
        }

        for c in prontos.iter().chain(descartar.iter()) {
            pendentes.remove(c);
        }

        if prontos.is_empty() {
            continue;
        }

        // 3) Organiza os prontos e registra no diário.
        let plano: Vec<AcaoPlanejada> = prontos
            .iter()
            .filter_map(|c| planejar_arquivo(c, config))
            .collect();

        if plano.is_empty() {
            continue;
        }

        let (lote, erros) = executar(&plano);

        for e in &erros {
            log(&format!("Aviso: {e}"));
        }
        for m in &lote.movimentos {
            log(&format!("{} -> {}", m.origem.display(), m.destino.display()));
        }

        if !lote.movimentos.is_empty() {
            let resultado = carregar_diario(diario).and_then(|mut d| {
                d.push(lote);
                salvar_diario(diario, &d)
            });
            if let Err(e) = resultado {
                log(&format!("ERRO ao gravar o diário: {e}"));
            }
        }
    }

    Ok(())
}