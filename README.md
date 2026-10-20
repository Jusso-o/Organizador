# Organizador

Aplicativo para revisar e organizar arquivos em pastas no Windows. A tela permite
escolher uma pasta de origem, visualizar os arquivos, aplicar regras por extensão,
ajustar os destinos antes de mover e desfazer o último lote.

## Iniciar no Windows

1. Execute `Organizador.exe` na pasta principal do projeto.
2. Escolha a pasta que deseja organizar.
3. Revise os arquivos e os destinos sugeridos; nada é movido antes de confirmar
   **Mover selecionados**.
4. Para reverter o último lote, use **Desfazer último lote**.

O aplicativo usa o WebView2 Runtime do Windows. Ele já vem instalado na maioria
das versões atuais do Windows; se o aplicativo não abrir, instale o [WebView2
Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).

As regras e o histórico ficam nos dados locais do aplicativo, separados da pasta
do executável. As regras podem ser editadas na própria tela.

## Executar a partir do código-fonte

Instale o Rust e os pré-requisitos de compilação do Tauri para Windows. Na pasta
principal do projeto, execute:

```powershell
cargo run --release -p organizador_app
```

Para gerar o executável:

```powershell
cargo build --release -p organizador_app
```

O build fica em `target\release\organizador_app.exe`. Para atualizar a cópia
distribuível na pasta principal, execute:

```powershell
Copy-Item target\release\organizador_app.exe Organizador.exe
```
