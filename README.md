# ollama-usage-tray

Um indicador de bandeja (system tray) leve para Linux/GNOME que monitora o consumo do seu plano **Ollama Cloud** na barra superior — créditos usados, sessão, requests e dias até a renovação. Tudo em Rust puro, sem GTK, ~11 MB de RAM.

![ícone](assets/icon.svg)

## Recursos

- **Título na bandeja**: `$34/$300` — dólares usados vs. créditos do plano
- **Ícone customizado** com anel de status (azul → laranja ≥70% → vermelho ≥90%)
- **Menu completo**: plano, créditos, uso (7 dias ou mês), sessão atual, requests por modelo, renovação
- **Detecção automática do modelo de preço**: contas antigas (limites de sessão/semana) e o novo modelo de créditos mensais em dólares são ambos suportados sem configuração
- **Gestão de API key**: `set-key`, `show-key`, `clear-key`, `key-info`; cópia da key (mascarada) direto do menu
- **Single-instance** (lock via `fd-lock`) e **autostart** no login
- Atualização a cada 5 minutos + item "Atualizar agora"

## Requisitos

- Linux com bandeja **StatusNotifierItem** (GNOME + extensão AppIndicator, KDE, XFCE, etc.)
- `libssl-dev`, `pkg-config` (build)
- Runtime: `python3` com `python3-gi` + `gir1.2-rsvg` (renderização do ícone), `xclip` (copiar key), `zenity` (diálogos), `nautilus` ou similar (abrir pasta)
- Conta Ollama com API key (a do opencode funciona: `~/.local/share/opencode/auth.json` → `ollama-cloud.key`)

## Instalação

```bash
git clone https://github.com/slipalison-ai/ollama-usage-tray.git
cd ollama-usage-tray
./install.sh
```

O script compila com `cargo build --release`, instala o binário em `~/.local/bin`, os assets em `~/.local/share/ollama-usage-tray/`, o ícone no tema hicolor, o autostart em `~/.config/autostart/` e lança o app.

### Desinstalação

```bash
./uninstall.sh          # no clone do repositório
# ou, para remover também a key local salva via set-key:
./uninstall.sh --purge
```

## Uso

O app fica na bandeja. Clique com o botão direito para o menu; passe o mouse para o tooltip com todos os detalhes.

### CLI

```bash
ollama-usage-tray                 # inicia o tray (segunda instância sai sozinha)
ollama-usage-tray key-info        # valida a key e imprime plano/uso no terminal
ollama-usage-tray set-key <KEY>   # salva a API key (ou via stdin: echo <KEY> | ollama-usage-tray set-key)
ollama-usage-tray show-key        # mostra a key mascarada + origem
ollama-usage-tray clear-key       # remove a key salva localmente
```

### Origem da API key (precedência)

1. `OLLAMA_USAGE_TRAY_KEY` (variável de ambiente)
2. `~/.config/ollama-usage-tray.key` (arquivo 0600, via `set-key`)
3. `~/.local/share/opencode/auth.json` (`ollama-cloud.key`) — integração com opencode

## Endpoints usados

| Endpoint | Uso |
|---|---|
| `GET https://ollama.com/api/usage` | `limits.session.usage`, `limits.weekly.usage` (frações), `activity.cost` (novo modelo mensal) |
| `POST https://ollama.com/api/me` | Plano (`pro`/`max`/`team`/`starter`) e data de criação (dia de renovação mensal) |

Créditos por plano: `pro` = $60/mês, `max` = $300/mês, `team` = $1000/mês (fonte: ollama.com/pricing).

## Segurança

- A key nunca aparece completa na UI (máscara `c10c••••Wpm9`); "Copiar API key" envia a key real para o clipboard via `xclip`
- O arquivo de key local é criado com modo `0600`
- Sem telemetria, sem dados para terceiros — o app só fala com `ollama.com`

## Licença

MIT