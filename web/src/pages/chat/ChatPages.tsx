// Коммуникации: список переписок слева, переписка справа (на телефоне — по очереди).
// Порядок — как в привычных мессенджерах: закреплённые сверху, затем по последнему сообщению.
import { Link, useParams } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { ChatItem, PresenceEntry } from "../../api/client";
import { formatShort } from "../../format";
import { IconBellOff, IconChat, IconPlus, IconSearch, IconSettings, IconStar } from "../../icons";
import { Dialog, Empty, ErrorBox, MenuButton, Spinner } from "../../ui";
import { UserAvatar } from "../people/presence";
import { usePresence } from "../people/profileHooks";
import { NewChatDialog } from "./ChatDialogs";
import { Conversation } from "./Conversation";
import { alertPrefs, saveAlertPrefs, useChats, type AlertPrefs } from "./chatUtil";

function ChatRow({ c, active, peer }: { c: ChatItem; active: boolean; peer?: PresenceEntry }) {
  const { t, i18n } = useTranslation();
  const preview = c.locked ? t("chat.locked") : c.preview || (c.preview_files > 0 ? `📎 ${t("chat.files", { count: c.preview_files })}` : c.kind === "group" ? t("chat.group") : t("chat.direct"));
  return (
    <li>
      <Link
        to="/chats/$id"
        params={{ id: c.id }}
        className={`mx-1.5 my-0.5 flex items-center gap-3 rounded-xl px-2.5 py-2 text-sm transition-colors ${active ? "bg-accent text-paper" : "hover:bg-hover"}`}
        aria-current={active ? "page" : undefined}
      >
        <UserAvatar name={c.title} userId={c.peer_id} avatar={peer?.avatar} presence={peer?.presence} size="md" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <span className={`min-w-0 flex-1 truncate ${c.unread > 0 ? "font-semibold" : "font-medium"}`}>{c.title}</span>
            {c.muted && (
              <>
                <IconBellOff size={13} className={active ? "text-paper/70" : "text-muted"} />
                <span className="sr-only">{t("chat.muted")}</span>
              </>
            )}
            {c.last_message_at && <span className={`shrink-0 text-[11px] tabular-nums ${active ? "text-paper/80" : c.unread > 0 && !c.muted ? "text-accent" : "text-muted"}`}>{formatShort(c.last_message_at, i18n.language)}</span>}
          </div>
          <div className="mt-0.5 flex items-center gap-1.5">
            <span className={`min-w-0 flex-1 truncate text-[13px] ${active ? "text-paper/80" : "text-muted"}`}>
              {!c.active && <span className="mr-1">{t("chat.readOnly")} ·</span>}
              {preview}
            </span>
            {c.unread > 0 && !c.locked ? (
              <span
                className={`min-w-5 shrink-0 rounded-full px-1.5 text-center text-xs leading-5 font-semibold tabular-nums ${active ? "bg-paper text-accent" : c.muted ? "bg-muted/40 text-paper" : "bg-accent text-paper"}`}
                aria-label={t("chat.unread", { count: c.unread })}
              >
                {c.unread > 999 ? "999+" : c.unread}
              </span>
            ) : (
              c.pinned && (
                <>
                  <IconStar size={13} className={active ? "text-paper/80" : "text-muted"} aria-hidden="true" />
                  <span className="sr-only">{t("chat.pinned")}</span>
                </>
              )
            )}
          </div>
        </div>
      </Link>
    </li>
  );
}

/** Звук и уведомления на рабочем столе — настройки этого браузера. */
function AlertsDialog({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [p, setP] = useState<AlertPrefs>(alertPrefs);
  const supported = typeof window !== "undefined" && "Notification" in window;
  const [permission, setPermission] = useState(supported ? Notification.permission : "denied");
  const change = (next: AlertPrefs) => {
    setP(next);
    saveAlertPrefs(next);
  };
  const desktop = async (on: boolean) => {
    if (on && supported && Notification.permission === "default") setPermission(await Notification.requestPermission());
    change({ ...p, desktop: on });
  };
  return (
    <Dialog open onClose={onClose} title={t("chat.alerts")}>
      <div className="grid gap-3 text-sm">
        <label className="flex items-start gap-3">
          <input type="checkbox" className="mt-0.5 size-4" checked={p.sound} onChange={(e) => change({ ...p, sound: e.target.checked })} />
          <span>
            <span className="block font-medium">{t("chat.alertSound")}</span>
            <span className="text-xs text-muted">{t("chat.alertSoundHint")}</span>
          </span>
        </label>
        <label className="flex items-start gap-3">
          <input type="checkbox" className="mt-0.5 size-4" disabled={!supported || permission === "denied"} checked={p.desktop && permission === "granted"} onChange={(e) => void desktop(e.target.checked)} />
          <span>
            <span className="block font-medium">{t("chat.alertDesktop")}</span>
            <span className="text-xs text-muted">{!supported ? t("chat.alertUnsupported") : permission === "denied" ? t("chat.alertDenied") : t("chat.alertDesktopHint")}</span>
          </span>
        </label>
        <p className="text-xs text-muted">{t("chat.alertMuteHint")}</p>
      </div>
    </Dialog>
  );
}

function ChatList({ current }: { current?: string }) {
  const { t } = useTranslation();
  const [hidden, setHidden] = useState(false);
  const [onlyUnread, setOnlyUnread] = useState(false);
  const [q, setQ] = useState("");
  const [creating, setCreating] = useState<"direct" | "group" | null>(null);
  const [alerts, setAlerts] = useState(false);
  const list = useChats(hidden);
  const peers = usePresence({ users: (list.data ?? []).flatMap((c) => (c.peer_id ? [c.peer_id] : [])) });
  const needle = q.trim().toLowerCase();
  const shown = (list.data ?? []).filter((c) => (!onlyUnread || (c.unread > 0 && !c.locked)) && (!needle || c.title.toLowerCase().includes(needle) || (c.preview ?? "").toLowerCase().includes(needle)));
  const unreadChats = (list.data ?? []).filter((c) => c.unread > 0 && !c.locked).length;
  const chip = (on: boolean) => `rounded-full px-3 py-1 text-xs font-medium transition-colors ${on ? "bg-accent text-paper" : "bg-sunken text-muted hover:text-ink"}`;
  return (
    <section className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-line bg-paper" aria-label={t("chat.title_list")}>
      <div className="flex items-center gap-2 px-3 pt-3 pb-2">
        <h2 className="min-w-0 flex-1 truncate text-lg font-semibold">{hidden ? t("chat.hiddenTitle") : t("chat.title_list")}</h2>
        <MenuButton
          iconOnly
          variant="ghost"
          label={t("chat.settings")}
          icon={<IconSettings size={18} />}
          items={[
            { key: "alerts", label: t("chat.alerts"), onSelect: () => setAlerts(true) },
            { key: "hidden", label: hidden ? t("chat.showMain") : t("chat.showHidden"), onSelect: () => setHidden(!hidden) },
          ]}
        />
        <MenuButton
          iconOnly
          variant="primary"
          label={t("chat.new")}
          icon={<IconPlus size={18} />}
          items={[
            { key: "direct", label: t("chat.newDirect"), onSelect: () => setCreating("direct") },
            { key: "group", label: t("chat.newGroup"), onSelect: () => setCreating("group") },
          ]}
        />
      </div>
      <div className="px-3 pb-2">
        <label className="flex items-center gap-2 rounded-full bg-sunken px-3 focus-within:ring-2 focus-within:ring-accent/40">
          <IconSearch size={16} className="text-muted" />
          <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("chat.searchChats")} aria-label={t("chat.searchChats")} className="min-w-0 flex-1 bg-transparent py-2 text-sm outline-none focus-visible:outline-none" />
        </label>
        {!hidden && (
          <div className="mt-2 flex gap-1.5" role="group" aria-label={t("chat.filter")}>
            <button type="button" className={chip(!onlyUnread)} aria-pressed={!onlyUnread} onClick={() => setOnlyUnread(false)}>
              {t("chat.all")}
            </button>
            <button type="button" className={chip(onlyUnread)} aria-pressed={onlyUnread} onClick={() => setOnlyUnread(true)}>
              {t("chat.unreadOnly")}
              {unreadChats > 0 && <span className="ml-1 tabular-nums">{unreadChats}</span>}
            </button>
          </div>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto pb-1.5">
        {list.error ? (
          <ErrorBox error={list.error} onRetry={() => void list.refetch()} />
        ) : list.isPending ? (
          <Spinner variant="list" />
        ) : list.data.length === 0 ? (
          <Empty icon={<IconChat size={28} />} hint={hidden ? undefined : t("chat.emptyHint")}>
            {hidden ? t("chat.noHidden") : t("chat.empty")}
          </Empty>
        ) : shown.length === 0 ? (
          <p className="px-4 py-6 text-center text-sm text-muted">{onlyUnread && !needle ? t("chat.allRead") : t("chat.nothingFound")}</p>
        ) : (
          <ul>
            {shown.map((c) => (
              <ChatRow key={c.id} c={c} active={c.id === current} peer={c.peer_id ? peers.byUser.get(c.peer_id) : undefined} />
            ))}
          </ul>
        )}
      </div>
      {hidden && (
        <button type="button" className="border-t border-line px-3 py-2 text-left text-xs text-accent" onClick={() => setHidden(false)}>
          {t("chat.showMain")}
        </button>
      )}
      {creating && <NewChatDialog mode={creating} onClose={() => setCreating(null)} />}
      {alerts && <AlertsDialog onClose={() => setAlerts(false)} />}
    </section>
  );
}

const shell = "grid h-[calc(100dvh-7.5rem)] min-h-96 gap-3 md:grid-cols-[21rem_1fr]";

export function ChatsPage() {
  const { t } = useTranslation();
  return (
    <div className={shell}>
      <ChatList />
      <div className="chat-wallpaper hidden items-center justify-center rounded-xl border border-line md:flex">
        <div className="rounded-2xl bg-paper/90 px-6 py-5 text-center shadow-sm">
          <IconChat size={36} className="mx-auto mb-2 text-accent" />
          <p className="font-medium">{t("chat.pick")}</p>
          <p className="mt-1 text-sm text-muted">{t("chat.pickHint")}</p>
        </div>
      </div>
    </div>
  );
}

export function ChatPage() {
  const { id } = useParams({ from: "/chats/$id" });
  return (
    <div className={shell}>
      <div className="hidden min-h-0 md:flex md:flex-col [&>*]:flex-1">
        <ChatList current={id} />
      </div>
      <Conversation key={id} id={id} />
    </div>
  );
}
