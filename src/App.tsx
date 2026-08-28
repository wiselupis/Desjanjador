import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

interface ExitInfo {
  addr: string;
  ip: string;
  country: string;
  latency_ms: number;
}

interface Status {
  active: boolean;
  autostart: boolean;
  proxy_api: boolean;
  use_tor: boolean;
  status: string;
  exit: ExitInfo | null;
  port: number;
}

interface Clients {
  betterdiscord: boolean;
  vencord: boolean;
  equicord: boolean;
  discord_installs: string[];
}

interface UpdateInfo {
  available: boolean;
  version: string;
  current: string;
  notes: string;
  url: string;
}

function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint: string;
}) {
  return (
    <div className="row">
      <div className="row-text">
        <div className="row-label">{label}</div>
        <div className="row-hint">{hint}</div>
      </div>
      <button
        className={`switch ${checked ? "on" : ""}`}
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
      >
        <span className="knob" />
      </button>
    </div>
  );
}

export default function App() {
  const [s, setS] = useState<Status | null>(null);
  const [busy, setBusy] = useState(false);
  const [clients, setClients] = useState<Clients | null>(null);
  const [action, setAction] = useState("");
  const [acting, setActing] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updating, setUpdating] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setS(await invoke<Status>("get_status"));
    } catch (e) {
      console.error(e);
    }
  }, []);

  const detect = useCallback(() => {
    invoke<Clients>("detect_clients").then(setClients).catch(console.error);
  }, []);

  useEffect(() => {
    refresh();
    detect();
    invoke<UpdateInfo>("check_update")
      .then((u) => {
        if (u.available) setUpdate(u);
      })
      .catch(console.error);
    const t = setInterval(refresh, 1500);
    return () => clearInterval(t);
  }, [refresh, detect]);

  const toggleActive = async (v: boolean) => {
    setBusy(true);
    try {
      setS(await invoke<Status>("set_active", { on: v }));
    } finally {
      setBusy(false);
    }
  };

  const toggleAuto = async (v: boolean) => {
    try {
      await invoke<boolean>("set_autostart", { on: v });
    } catch (e) {
      console.error(e);
    }
    refresh();
  };

  const toggleApi = async (v: boolean) => {
    try {
      setS(await invoke<Status>("set_proxy_api", { on: v }));
    } catch (e) {
      console.error(e);
    }
  };

  const toggleTor = async (v: boolean) => {
    try {
      setS(await invoke<Status>("set_use_tor", { on: v }));
    } catch (e) {
      console.error(e);
    }
  };

  const doRefreshExit = async () => {
    setRefreshing(true);
    try {
      setS(await invoke<Status>("refresh_exit"));
    } catch (e) {
      console.error(e);
    } finally {
      setTimeout(() => setRefreshing(false), 900);
    }
  };

  const installBD = async () => {
    setActing(true);
    setAction("Instalando BetterDiscord…");
    try {
      setAction(await invoke<string>("install_betterdiscord"));
    } catch (e) {
      setAction("Erro: " + e);
    } finally {
      setActing(false);
      detect();
    }
  };

  const patchClient = async () => {
    setActing(true);
    setAction("Aplicando patch…");
    try {
      setAction(await invoke<string>("patch_client"));
    } catch (e) {
      setAction("Erro: " + e);
    } finally {
      setActing(false);
    }
  };

  const applyUpdate = async () => {
    if (!update) return;
    setUpdating(true);
    try {
      await invoke("apply_update", { url: update.url });
      // The app replaces its exe and relaunches; this window closes.
    } catch (e) {
      setUpdating(false);
      alert("Falha ao atualizar: " + e);
    }
  };

  const doExit = async () => {
    await invoke("exit_app");
  };

  const active = s?.active ?? false;
  const noMod = clients && !clients.betterdiscord && !clients.vencord && !clients.equicord;

  return (
    <main className="app">
      <img
        className="mascot"
        src="/mascot.png"
        alt=""
        onError={(e) => {
          e.currentTarget.style.display = "none";
        }}
      />
      <div className="content">
      <header className="head">
        <h1>Desjanjador</h1>
        <span className={`dot ${active ? "live" : ""}`} />
      </header>
      <p className="tagline">Desbloqueia Go&nbsp;Live e câmera do Discord no Brasil</p>

      <section className="card">
        <Toggle
          label="Ativo"
          hint="Roteia o gateway do Discord por uma saída fora do Brasil"
          checked={active}
          onChange={toggleActive}
        />
        <div className="divider" />
        <Toggle
          label="Iniciar com o Windows"
          hint="Abre na bandeja ao ligar o PC"
          checked={s?.autostart ?? false}
          onChange={toggleAuto}
        />
        <div className="divider" />
        <Toggle
          label="Desbloquear canais +18"
          hint="Roteia também a API do Discord (as ações podem ficar um pouco mais lentas)"
          checked={s?.proxy_api ?? false}
          onChange={toggleApi}
        />
        <div className="divider" />
        <Toggle
          label="Usar Tor (opcional)"
          hint="Se você tiver o Tor aberto, usa como saída reserva. Desligado por padrão."
          checked={s?.use_tor ?? false}
          onChange={toggleTor}
        />
      </section>

      <section className={`status ${active ? "status-on" : ""}`}>
        <div className="status-top">
          <div className="status-line">{busy ? "processando…" : s?.status ?? "…"}</div>
          <button
            className={`refresh ${refreshing ? "spin" : ""}`}
            title="Trocar de saída"
            aria-label="Trocar de saída"
            disabled={!active || refreshing}
            onClick={doRefreshExit}
          >
            ↻
          </button>
        </div>
        {s?.exit ? (
          <div className="exit">
            saída <b>{s.exit.country}</b> · {s.exit.ip}
            {s.exit.latency_ms > 0 && <span className="lat"> · {s.exit.latency_ms} ms</span>}
            <span className="mono"> ({s.exit.addr})</span>
          </div>
        ) : (
          <div className="exit muted">nenhuma saída selecionada</div>
        )}
        <div className="port">proxy local · 127.0.0.1:{s?.port ?? "—"}</div>
      </section>

      <section className="help">
        <b>Como usar</b>
        <ol>
          <li>Ligue <b>Ativo</b> e espere a saída ficar pronta.</li>
          <li>Reinicie o Discord (Stable / PTB / Canary).</li>
          <li>Entre numa call de voz e teste o Go&nbsp;Live / câmera.</li>
        </ol>
        <span className="tip">
          Saída automática entre proxies rápidos em países sem verificação de idade;
          mantém reservas testadas e troca sozinha se cair. O Tor é opcional
          (ligue acima se quiser usá-lo como reserva).
        </span>
      </section>

      <section className="card patch">
        <div className="patch-head">
          <span>Patch do cliente</span>
          <span className="mods">
            {clients?.betterdiscord && <b>BetterDiscord</b>}
            {clients?.vencord && <b>Vencord</b>}
            {clients?.equicord && <b>Equicord</b>}
            {noMod && <span className="muted">nenhum detectado</span>}
          </span>
        </div>
        <div className="patch-note">
          <b>Recomendado.</b> Precisa de um cliente custom (BetterDiscord ou Vencord).
        </div>
        <div className="btn-row">
          <button className="mini" disabled={acting} onClick={installBD}>
            Instalar BetterDiscord
          </button>
          <button className="mini" disabled={acting} onClick={patchClient}>
            Aplicar patch
          </button>
        </div>
        {action && <div className="action-msg">{action}</div>}
      </section>
      </div>

      <button className="exit-btn" onClick={doExit}>
        Sair
      </button>
      <p className="foot">
        Fechar a janela mantém na bandeja. Reinicie o Discord após ativar.
      </p>

      {update && (
        <div className="modal-overlay">
          <div className="modal">
            <h2>Nova versão disponível</h2>
            <p className="modal-ver">
              {update.current} → <b>{update.version}</b>
            </p>
            {update.notes && <p className="modal-notes">{update.notes}</p>}
            <div className="modal-btns">
              <button className="mini ghost" disabled={updating} onClick={() => setUpdate(null)}>
                Pular
              </button>
              <button className="mini primary" disabled={updating} onClick={applyUpdate}>
                {updating ? "Atualizando…" : "Atualizar"}
              </button>
            </div>
          </div>
        </div>
      )}
    </main>
  );
}
