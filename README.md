# Desjanjador

Desbloqueia o **Go Live e a câmera do Discord no Brasil**. Roteia **só o gateway** do
Discord por uma saída fora do Brasil (proxy gratuito testado ou Tor); todo o resto sai
direto, em velocidade normal. App em **Tauri** (Rust + React), Windows, portable (sem
assinatura).

## Como usar

1. Rode `install.ps1` (ou abra o `desjanjador.exe` direto).
2. Ligue **Ativo** e espere a saída ficar pronta.
3. **Reinicie o Discord** (Stable / PTB / Canary) — o gateway precisa nascer pela saída.
4. Entre numa call de voz e teste o **Go Live / câmera**.

> Dica: com o **Tor Browser** aberto, a saída fica bem mais estável que proxy gratuito
> (proxy grátis costuma cair a cada ~40s e derrubar o gateway).

## Botões

- **Instalar BetterDiscord** — injeta o BetterDiscord em qualquer Discord instalado.
- **Aplicar patch** — coloca `Desjanjador.plugin.js` no BetterDiscord (rede de
  segurança; o desbloqueio de verdade é o proxy do gateway).

## Instalar / iniciar com o Windows

```powershell
.\install.ps1
```

- Copia o exe para `%LOCALAPPDATA%\Desjanjador`, tira a "marca da web" (`Unblock-File`)
  e liga a inicialização com o Windows (chave Run).
- Rodar **como Administrador** uma vez adiciona uma exclusão no Defender (opcional).
- Remover: `.\install.ps1 -Uninstall`.

## Aviso

Não é assinado — na 1ª execução o SmartScreen pode avisar ("Mais informações →
Executar assim mesmo"). Windows portable **não precisa** de assinatura. Usar isto pode
violar os termos do Discord; use por conta própria.
