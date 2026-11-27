// Переписка как в привычных мессенджерах: пузыри с хвостиком на фоне с узором, картинки мозаикой,
// разделители по дням, «Новые сообщения», ответы и пересылка, реакции, закреплённое сверху, «печатает…»,
// ✓/✓✓ прочитано, поиск по переписке и все её файлы, «Мои GIF». Лента не прыгает вниз, пока человек
// читает старое: внизу появляется кнопка со счётчиком. На телефоне переписка открывается во весь экран.
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { api, ok, type ChatDetail, type ChatMessage, type ChatSharedFile } from "../../api/client";
import { formatDate, formatDay, formatTime, tone } from "../../format";
import {
  IconArrowLeft,
  IconBell,
  IconBellOff,
  IconCheck,
  IconCheckDouble,
  IconChevronDown,
  IconCopy,
  IconFileText,
  IconMore,
  IconPin,
  IconPlus,
  IconReply,
  IconSearch,
  IconSmile,
  IconStar,
  IconTransfer,
  IconX,
} from "../../icons";
import { Lightbox } from "../../lightbox";
import { useMe } from "../../session";
import { toast } from "../../toast";
import { Empty, ErrorBox, Input, MenuButton, Spinner } from "../../ui";
import { UserAvatar } from "../people/presence";
import { usePresence } from "../people/profileHooks";
import { formatSize } from "../helpdesk/helpdeskUtil";
import { ChatInfo, ForwardDialog, Versions } from "./ChatDialogs";
import { Documents, Media, MessageText, Quote, ReactionPicker, Reactions } from "./ChatParts";
import { Composer } from "./Composer";
import { chatFileUrl, dayKey, onlyEmoji, uploadPending, MAX_FILES, useTyping } from "./chatUtil";

/** Подряд идущие сообщения одного автора в пределах 5 минут — одной группой (имя и аватар один раз). */
const RUN_MS = 5 * 60_000;
/** Сколько страниц истории догружать, чтобы дойти до сообщения из поиска или закреплённого. */
const JUMP_PAGES = 30;
/** Сколько после открытия лента держится у «Новых сообщений» (или внизу), пока догружаются картинки. */
const SETTLE_MS = 2500;
const settleDeadline = () => Date.now() + SETTLE_MS;

/** Прокрутить ленту к элементу: `start` — к верху, `center` — в середину. Страница вокруг не двигается. */
function scrollWithin(scroller: HTMLElement, target: HTMLElement, block: "start" | "center", smooth = false) {
  const s = scroller.getBoundingClientRect();
  const r = target.getBoundingClientRect();
  const shift = block === "center" ? (s.height - r.height) / 2 : 0;
  scroller.scrollTo({ top: scroller.scrollTop + r.top - s.top - shift, behavior: smooth ? "smooth" : "auto" });
}

const narrow = () => typeof window !== "undefined" && window.matchMedia("(max-width: 639px)").matches;

const hasFiles = (e: { dataTransfer: DataTransfer }) => [...e.dataTransfer.types].includes("Files");

type Panel = "search" | "files" | null;

export function Conversation({ id }: { id: string }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const me = useMe().data;
  const [info, setInfo] = useState(false);
  const [panel, setPanel] = useState<Panel>(null);
  const [editing, setEditing] = useState<ChatMessage | null>(null);
  const [reply, setReply] = useState<ChatMessage | null>(null);
  const [versions, setVersions] = useState<ChatMessage | null>(null);
  const [forward, setForward] = useState<ChatMessage | null>(null);
  const [dragging, setDragging] = useState(false);
  const detail = useQuery({ queryKey: ["chat", id], queryFn: () => ok<ChatDetail>(api.GET("/api/chat/{id}", { params: { path: { id } } })) });
  const locked = detail.data?.chat.locked === true;
  const messages = useInfiniteQuery({
    queryKey: ["chat-messages", id],
    queryFn: ({ pageParam }) => ok<ChatMessage[]>(api.GET("/api/chat/{id}/messages", { params: { path: { id }, query: { before: pageParam } } })),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => (last.length >= 100 ? last[0]?.id : undefined),
    enabled: detail.isSuccess && !locked,
  });
  const all = [...(messages.data?.pages ?? [])].reverse().flat();
  const newest = all[all.length - 1];
  const unread = detail.data?.chat.unread ?? 0;

  // Открытая переписка прочитана: и при входе, и когда приходит новое сообщение (если вкладка на экране).
  const read = useMutation({
    mutationFn: () => ok(api.POST("/api/chat/{id}/read", { params: { path: { id } } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat", id] });
    },
  });
  const readMutate = read.mutate;
  useEffect(() => {
    const back = () => {
      if (document.visibilityState === "visible" && (qc.getQueryData<ChatDetail>(["chat", id])?.chat.unread ?? 0) > 0) readMutate();
    };
    document.addEventListener("visibilitychange", back);
    return () => document.removeEventListener("visibilitychange", back);
  }, [id, qc, readMutate]);

  // Граница «Новые сообщения» — по непрочитанному в момент открытия (запоминается при первом показе).
  const [unreadAtOpen, setUnreadAtOpen] = useState<number | null>(null);
  if (unreadAtOpen === null && detail.data && detail.isFetchedAfterMount) setUnreadAtOpen(detail.data.chat.unread);
  const [firstUnread, setFirstUnread] = useState<string | null | undefined>(undefined);
  if (firstUnread === undefined && unreadAtOpen !== null && messages.isSuccess && messages.isFetchedAfterMount && !messages.isFetching && me) {
    let left = unreadAtOpen;
    let found: string | null = null;
    for (let i = all.length - 1; i >= 0 && left > 0; i--) {
      const m = all[i];
      if (m && m.author_id !== me.user_id) {
        left--;
        found = m.id;
      }
    }
    setFirstUnread(found);
  }
  const latched = firstUnread !== undefined;
  useEffect(() => {
    if (latched && newest && unread > 0 && document.visibilityState === "visible") readMutate();
  }, [latched, newest, unread, readMutate]);

  // ---------------------------------------------------------------- прокрутка
  const scroller = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLOListElement>(null);
  const nearBottom = useRef(true);
  const started = useRef(false);
  // До этого времени лента держится у начальной точки (картинки догружаются и меняют высоту).
  const settleUntil = useRef(0);
  const jumping = useRef(false);
  const lastNewest = useRef<string | undefined>(undefined);
  const heightBefore = useRef<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  // Последнее сообщение, до которого человек долистал: всё, что новее, — на счётчике кнопки «вниз».
  const [seen, setSeen] = useState<string | undefined>(undefined);
  const seenAt = seen ? all.findIndex((m) => m.id === seen) : -1;
  const missed = atBottom || seenAt < 0 ? 0 : all.slice(seenAt + 1).filter((m) => m.author_id !== me?.user_id).length;

  const settle = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const mark = document.getElementById("chat-unread-mark");
    if (mark) scrollWithin(el, mark, "start");
    else el.scrollTop = el.scrollHeight;
  }, []);

  // Первый показ: к «Новым сообщениям» или в самый низ.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (started.current || !el || firstUnread === undefined || all.length === 0) return;
    started.current = true;
    lastNewest.current = newest?.id;
    settleUntil.current = settleDeadline();
    settle();
  }, [all.length, firstUnread, newest?.id, settle]);

  // Высота ленты изменилась (догрузилась картинка, раскрылся код): держим начальную точку или низ.
  const hasList = all.length > 0;
  useEffect(() => {
    const el = scroller.current;
    const ol = list.current;
    if (!hasList || !el || !ol || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (!started.current || jumping.current || heightBefore.current !== null) return;
      if (Date.now() < settleUntil.current) settle();
      else if (nearBottom.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(ol);
    ro.observe(el);
    return () => ro.disconnect();
  }, [hasList, settle]);

  // Новое сообщение: вниз, если человек и так внизу или написал сам; иначе — счётчик на кнопке.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!started.current || !el || !newest || newest.id === lastNewest.current) return;
    lastNewest.current = newest.id;
    settleUntil.current = 0;
    if (nearBottom.current || newest.author_id === me?.user_id) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [newest, me?.user_id]);

  // Подгрузили старые сообщения сверху — лента остаётся на том же месте.
  const pageCount = messages.data?.pages.length ?? 0;
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && heightBefore.current !== null) {
      el.scrollTop += el.scrollHeight - heightBefore.current;
      heightBefore.current = null;
    }
  }, [pageCount]);

  const toBottom = () => {
    const el = scroller.current;
    if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  // Человек сам крутит ленту — начальная точка больше не держится.
  const userScroll = () => {
    settleUntil.current = 0;
  };

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    nearBottom.current = bottom;
    if (bottom !== atBottom) setAtBottom(bottom);
    if (bottom && newest && newest.id !== seen) setSeen(newest.id);
    if (!bottom && seen === undefined && newest) setSeen(newest.id);
    // У верхнего края — догрузить старое.
    if (el.scrollTop < 200 && started.current && !jumping.current && messages.hasNextPage && !messages.isFetchingNextPage) {
      heightBefore.current = el.scrollHeight;
      void messages.fetchNextPage().then((r) => {
        if (r.isError) heightBefore.current = null;
      });
    }
  };

  // Переход к сообщению (поиск, закреплённое, цитата): догружаем историю, пока не найдётся.
  const [flash, setFlash] = useState<string | null>(null);
  const { fetchNextPage } = messages;
  const jumpTo = async (mid: string) => {
    if (!mid || jumping.current) return;
    jumping.current = true;
    settleUntil.current = 0;
    try {
      let more = messages.hasNextPage;
      for (let n = 0; n <= JUMP_PAGES; n++) {
        const el = document.getElementById(`msg-${mid}`);
        if (el && scroller.current) {
          scrollWithin(scroller.current, el, "center", true);
          setFlash(mid);
          window.setTimeout(() => setFlash((f) => (f === mid ? null : f)), 1800);
          return;
        }
        if (!more) break;
        heightBefore.current = null;
        const r = await fetchNextPage();
        more = r.hasNextPage;
        await new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done())));
      }
      toast(t("chat.notLoaded"), "bad");
    } finally {
      jumping.current = false;
    }
  };
  const jump = (mid: string) => void jumpTo(mid);
  const jumpFromPanel = (mid: string) => {
    if (narrow()) setPanel(null);
    jump(mid);
  };

  // ---------------------------------------------------------------- действия
  const refreshMessages = () => void qc.invalidateQueries({ queryKey: ["chat-messages", id] });
  const fail = (e: unknown) => toast((e instanceof Error && e.message) || t("common.error"), "bad");
  const prefs = useMutation({
    mutationFn: (body: { pinned?: boolean; hidden?: boolean; muted?: boolean }) => ok(api.PUT("/api/chat/{id}/prefs", { params: { path: { id } }, body })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat", id] });
    },
  });
  const react = useMutation({
    mutationFn: ({ message, emoji }: { message: string; emoji: string }) =>
      ok(api.PUT("/api/chat/{id}/messages/{message}/reactions", { params: { path: { id, message } }, body: { emoji } })),
    onSuccess: refreshMessages,
    onError: fail,
  });
  const pin = useMutation({
    mutationFn: (message: string | null) => ok(api.PUT("/api/chat/{id}/pin", { params: { path: { id } }, body: { message } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["chat", id] }),
    onError: fail,
  });
  const saveGif = useMutation({
    mutationFn: (file: string) => ok(api.POST("/api/chat/gifs/from-file", { body: { file } })),
    onSuccess: () => {
      toast(t("chat.gifSaved"));
      void qc.invalidateQueries({ queryKey: ["chat-gifs"] });
    },
    onError: fail,
  });

  const members = detail.data?.members ?? [];
  const typingIds = useTyping(id).filter((u) => u !== me?.user_id);
  const memberInfo = usePresence({ users: detail.data?.chat.kind === "group" ? members.map((m) => m.user_id) : detail.data?.chat.peer_id ? [detail.data.chat.peer_id] : [] });

  const frame = "relative flex min-h-0 flex-col overflow-hidden bg-paper md:rounded-xl md:border md:border-line max-md:fixed max-md:inset-0 max-md:z-40";
  const back = (
    <Link to="/chats" className="m-2 self-start rounded-full p-2 text-muted hover:bg-hover md:hidden" aria-label={t("chat.back")}>
      <IconArrowLeft size={20} />
    </Link>
  );
  if (detail.error)
    return (
      <section className={frame}>
        {back}
        <ErrorBox error={detail.error} />
      </section>
    );
  if (detail.isPending)
    return (
      <section className={frame}>
        {back}
        <Spinner />
      </section>
    );
  const d = detail.data;
  const c = d.chat;
  const group = c.kind === "group";
  const peer = c.peer_id ? memberInfo.byUser.get(c.peer_id) : undefined;
  const manager = c.active && (c.role === "owner" || c.role === "admin");
  const readUpto = d.read_upto ? new Date(d.read_upto).getTime() : 0;
  const typingNames = typingIds.map((u) => members.find((m) => m.user_id === u)?.name ?? "…");
  const incoming = newest && newest.author_id !== me?.user_id ? `${newest.author_name}: ${newest.body.split("\n")[0] ?? ""}` : "";

  const subtitle = typingNames.length
    ? group
      ? t("chat.typingNamed", { names: typingNames.slice(0, 2).join(", "), count: typingNames.length })
      : t("chat.typing")
    : group
      ? c.active
        ? t("chat.membersN", { n: members.length })
        : t("chat.readOnly")
      : peer && (peer.status_emoji || peer.status_text)
        ? `${peer.status_emoji ?? ""} ${peer.status_text ?? ""}`.trim()
        : peer
          ? t(`people.presence.${peer.presence}`)
          : t("chat.direct");
  const online = !group && peer?.presence === "online" && !typingNames.length;

  const canWrite = d.can_write && !locked;
  const addFiles = (list: File[]) => {
    if (canWrite && !editing && list.length > 0) void uploadPending(id, list, { tooMany: t("chat.tooManyFiles", { count: MAX_FILES }), failed: t("common.error") });
  };
  const editLast = () => {
    const mine = [...all].reverse().find((m) => m.author_id === me?.user_id && m.editable && m.body);
    if (mine) setEditing(mine);
  };
  const headButton = (on: boolean) => `rounded-full p-2 transition-colors hover:bg-hover ${on ? "bg-hover text-accent" : "text-muted hover:text-ink"}`;

  return (
    <section
      className={frame}
      aria-label={c.title}
      onDragEnter={(e) => {
        if (hasFiles(e) && canWrite && !editing) setDragging(true);
      }}
      onDragOver={(e) => {
        if (hasFiles(e)) e.preventDefault();
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target || !e.currentTarget.contains(e.relatedTarget as Node)) setDragging(false);
      }}
      onDrop={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        setDragging(false);
        addFiles([...e.dataTransfer.files]);
      }}
    >
      {/* Шапка */}
      <header className="z-10 flex items-center gap-3 border-b border-line bg-paper px-2 py-2 sm:px-3">
        <Link to="/chats" className="rounded-full p-2 text-muted hover:bg-hover md:hidden" aria-label={t("chat.back")}>
          <IconArrowLeft size={20} />
        </Link>
        <UserAvatar name={c.title} userId={c.peer_id ?? c.id} avatar={peer?.avatar} presence={peer?.presence} size="md" />
        <div className="min-w-0 flex-1 leading-tight">
          {c.peer_id ? (
            <Link to="/people/$id" params={{ id: c.peer_id }} className="block truncate text-[15px] font-semibold hover:underline">
              {c.title}
            </Link>
          ) : group && c.active && !locked ? (
            <button type="button" className="block max-w-full truncate text-left text-[15px] font-semibold hover:underline" onClick={() => setInfo(true)}>
              {c.title}
            </button>
          ) : (
            <h2 className="truncate text-[15px] font-semibold">{c.title}</h2>
          )}
          <div className={`mt-0.5 truncate text-[13px] ${typingNames.length || online ? "text-accent" : "text-muted"}`} aria-live="polite">
            {typingNames.length > 0 && <span className="chat-typing-dots mr-1" aria-hidden="true" />}
            {subtitle}
          </div>
        </div>
        {!locked && (
          <>
            <button type="button" className={headButton(panel === "search")} aria-pressed={panel === "search"} aria-label={t("chat.searchInChat")} title={t("chat.searchInChat")} onClick={() => setPanel(panel === "search" ? null : "search")}>
              <IconSearch size={19} />
            </button>
            <button type="button" className={`${headButton(panel === "files")} max-sm:hidden`} aria-pressed={panel === "files"} aria-label={t("chat.sharedFiles")} title={t("chat.sharedFiles")} onClick={() => setPanel(panel === "files" ? null : "files")}>
              <IconFileText size={19} />
            </button>
          </>
        )}
        <MenuButton
          iconOnly
          variant="ghost"
          label={t("common.more")}
          icon={<IconMore size={19} />}
          items={[
            ...(!locked ? [{ key: "files", label: t("chat.sharedFiles"), icon: <IconFileText size={16} />, onSelect: () => setPanel("files") }] : []),
            { key: "mute", label: c.muted ? t("chat.unmute") : t("chat.mute"), icon: c.muted ? <IconBell size={16} /> : <IconBellOff size={16} />, onSelect: () => prefs.mutate({ muted: !c.muted }) },
            ...(c.kind === "direct" || c.active ? [{ key: "pin", label: c.pinned ? t("chat.unpin") : t("chat.pin"), icon: <IconStar size={16} />, onSelect: () => prefs.mutate({ pinned: !c.pinned }) }] : []),
            ...(group && !c.active ? [{ key: "hide", label: c.hidden ? t("chat.unhide") : t("chat.hide"), onSelect: () => prefs.mutate({ hidden: !c.hidden }) }] : []),
            ...(group && c.active && !locked ? [{ key: "info", label: t("chat.info"), onSelect: () => setInfo(true), separated: true }] : []),
          ]}
        />
      </header>
      {prefs.error && <ErrorBox error={prefs.error} />}

      {/* Закреплённое */}
      {d.pinned_message && !locked && (
        <div className="z-10 flex items-center gap-2 border-b border-line bg-paper/95 px-3 py-1.5 backdrop-blur">
          <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-accentsoft text-accent">
            <IconPin size={15} />
          </span>
          <div className="min-w-0 flex-1">
            <Quote q={d.pinned_message} label={t("chat.pinnedMessage")} onClick={() => jump(d.pinned_message?.id ?? "")} onClear={d.can_pin ? () => pin.mutate(null) : undefined} />
          </div>
        </div>
      )}

      {locked ? (
        <Empty hint={t("chat.lockedHint")}>{t("chat.locked")}</Empty>
      ) : (
        <div className="relative flex min-h-0 flex-1">
          <div className="chat-wallpaper relative flex min-w-0 flex-1 flex-col">
            <div ref={scroller} onScroll={onScroll} onWheel={userScroll} onTouchStart={userScroll} onKeyDown={userScroll} className="min-h-0 flex-1 overflow-y-auto px-2 pt-3 pb-1 [overflow-anchor:none] focus-visible:outline-none sm:px-4" role="log" tabIndex={0} aria-label={t("chat.messages")}>
              {messages.isFetchingNextPage && (
                <div className="mb-2 flex justify-center" role="status">
                  <span className="rounded-full bg-black/30 px-3 py-0.5 text-xs text-white backdrop-blur-sm">{t("chat.loadingOlder")}</span>
                </div>
              )}
              {messages.error ? (
                <ErrorBox error={messages.error} />
              ) : messages.isPending ? (
                <Spinner />
              ) : all.length === 0 ? (
                <div className="flex h-full items-center justify-center">
                  <div className="max-w-xs rounded-3xl bg-paper/90 px-7 py-6 text-center shadow-sm backdrop-blur">
                    <div className="emoji mb-2 text-5xl" aria-hidden="true">
                      👋
                    </div>
                    <p className="font-semibold">{t("chat.noMessages")}</p>
                    <p className="mt-1 text-sm text-muted">{t("chat.sayHello")}</p>
                  </div>
                </div>
              ) : (
                <ol ref={list} className="mx-auto max-w-3xl pb-1">
                  {all.map((m, i) => {
                    const prev = all[i - 1];
                    const next = all[i + 1];
                    const newDay = !prev || dayKey(prev.created_at) !== dayKey(m.created_at);
                    const runStart = newDay || !prev || prev.author_id !== m.author_id || new Date(m.created_at).getTime() - new Date(prev.created_at).getTime() > RUN_MS || m.id === firstUnread;
                    const runEnd = !next || next.author_id !== m.author_id || dayKey(next.created_at) !== dayKey(m.created_at) || new Date(next.created_at).getTime() - new Date(m.created_at).getTime() > RUN_MS || next.id === firstUnread;
                    const mine = m.author_id === me?.user_id;
                    const gif = m.files.find((f) => f.mime === "image/gif" && !f.expired);
                    return (
                      <li key={m.id}>
                        {newDay && (
                          <div className="my-3 flex justify-center">
                            <span className="rounded-full bg-black/30 px-3 py-1 text-xs font-medium text-white backdrop-blur-sm">{formatDay(m.created_at, i18n.language)}</span>
                          </div>
                        )}
                        {m.id === firstUnread && (
                          <div id="chat-unread-mark" className="-mx-2 my-3 scroll-mt-12 bg-paper/75 py-1 text-center text-[13px] font-medium text-accent backdrop-blur-sm sm:-mx-4">
                            {t("chat.newMessages")}
                          </div>
                        )}
                        <Bubble
                          m={m}
                          mine={mine}
                          group={group}
                          runStart={runStart}
                          runEnd={runEnd}
                          flash={flash === m.id}
                          read={mine && new Date(m.created_at).getTime() <= readUpto}
                          avatar={memberInfo.byUser.get(m.author_id)?.avatar}
                          canWrite={canWrite}
                          canPin={d.can_pin}
                          pinned={d.pinned_message?.id === m.id}
                          canVersions={!group || mine || manager}
                          onReply={() => {
                            setEditing(null);
                            setReply(m);
                          }}
                          onEdit={() => {
                            setReply(null);
                            setEditing(m);
                          }}
                          onVersions={() => setVersions(m)}
                          onForward={() => setForward(m)}
                          onPin={() => pin.mutate(d.pinned_message?.id === m.id ? null : m.id)}
                          onReact={(emoji) => react.mutate({ message: m.id, emoji })}
                          onSaveGif={gif ? () => saveGif.mutate(gif.id) : undefined}
                          onJump={jump}
                        />
                      </li>
                    );
                  })}
                </ol>
              )}
            </div>
            <p className="sr-only" aria-live="polite">
              {incoming}
            </p>
            {!atBottom && (
              <button type="button" onClick={() => toBottom()} className="absolute right-3 bottom-[5.25rem] z-10 flex size-11 animate-pop-in items-center justify-center rounded-full bg-paper text-muted shadow-lg ring-1 ring-line hover:text-ink sm:right-5" aria-label={missed ? t("chat.newBelow", { count: missed }) : t("chat.toBottom")}>
                <IconChevronDown size={22} />
                {missed > 0 && <span className="absolute -top-1.5 -right-1 min-w-5 rounded-full bg-accent px-1 text-center text-xs leading-5 font-semibold text-paper tabular-nums">{missed}</span>}
              </button>
            )}
            {canWrite ? (
              <Composer
                key={editing?.id ?? "new"}
                chat={id}
                editing={editing}
                reply={reply}
                onAddFiles={addFiles}
                onEditLast={editLast}
                onJump={jump}
                onDone={() => {
                  setEditing(null);
                  setReply(null);
                }}
                onCancelReply={() => setReply(null)}
              />
            ) : (
              <p className="m-2 rounded-2xl bg-paper/90 px-3 py-3 text-center text-sm text-muted backdrop-blur sm:mx-4 sm:mb-3">{t("chat.readOnlyHint")}</p>
            )}
            {dragging && (
              <div className="pointer-events-none absolute inset-3 z-20 flex flex-col items-center justify-center gap-2 rounded-3xl border-2 border-dashed border-accent bg-paper/90 text-lg font-medium text-accent backdrop-blur">
                <IconPlus size={28} />
                {t("chat.dropHere")}
              </div>
            )}
          </div>
          {panel === "search" && <SearchPanel chat={id} onClose={() => setPanel(null)} onJump={jumpFromPanel} />}
          {panel === "files" && <FilesPanel chat={id} onClose={() => setPanel(null)} onJump={jumpFromPanel} />}
        </div>
      )}
      {info && <ChatInfo d={d} onClose={() => setInfo(false)} />}
      {versions && <Versions chat={id} message={versions} onClose={() => setVersions(null)} />}
      {forward && <ForwardDialog chat={id} message={forward} onClose={() => setForward(null)} />}
    </section>
  );
}

// ------------------------------------------------------------------ Сообщение

type BubbleProps = {
  m: ChatMessage;
  mine: boolean;
  group: boolean;
  runStart: boolean;
  runEnd: boolean;
  flash: boolean;
  read: boolean;
  avatar?: string | null;
  canWrite: boolean;
  canPin: boolean;
  pinned: boolean;
  canVersions: boolean;
  onReply: () => void;
  onEdit: () => void;
  onVersions: () => void;
  onForward: () => void;
  onPin: () => void;
  onReact: (emoji: string) => void;
  onSaveGif?: () => void;
  onJump: (id: string) => void;
};

function Bubble(p: BubbleProps) {
  const { m, mine, group } = p;
  const { t, i18n } = useTranslation();
  const [picking, setPicking] = useState(false);
  // Выбор реакции рисуется поверх страницы у кнопки (или у сообщения): лента его не обрезает.
  const [pickAt, setPickAt] = useState<CSSProperties>({});
  const smile = useRef<HTMLButtonElement>(null);
  const pickBox = useRef<HTMLDivElement>(null);
  const closePicker = useCallback(() => setPicking(false), []);
  const openPicker = (r: DOMRect | undefined) => {
    if (!r) return;
    const up = r.top > 64;
    const w = Math.min(288, window.innerWidth - 16);
    const left = mine ? r.right - w : r.left;
    setPickAt({
      ...(up ? { bottom: window.innerHeight - r.top + 6 } : { top: r.bottom + 6 }),
      left: Math.max(8, Math.min(left, window.innerWidth - w - 8)),
      width: w,
    });
    setPicking(true);
  };
  // Лента прокрутилась — выбор закрывается, чтобы не висеть отдельно от сообщения.
  useEffect(() => {
    if (!picking) return;
    const moved = (e: Event) => {
      if (!(e.target instanceof Node && pickBox.current?.contains(e.target))) setPicking(false);
    };
    window.addEventListener("scroll", moved, true);
    return () => window.removeEventListener("scroll", moved, true);
  }, [picking]);
  const images = m.files.filter((f) => f.image && !f.expired);
  const docs = m.files.filter((f) => !f.image || f.expired);
  const big = !m.reply && !m.forwarded_from_name && m.files.length === 0 && onlyEmoji(m.body);
  // Только картинки без подписи — без пузыря, время поверх картинки.
  const bare = images.length > 0 && docs.length === 0 && !m.body && !m.reply && !m.forwarded_from_name;
  const showName = group && !mine && p.runStart && !big;
  const showAvatar = group && !mine;
  const tail = p.runEnd && !big && !bare;
  const who = `tone-${tone(m.author_id)}`;
  const copy = () => {
    if (!navigator.clipboard) {
      toast(t("common.error"), "bad");
      return;
    }
    void navigator.clipboard.writeText(m.body).then(
      () => toast(t("chat.copied")),
      () => toast(t("common.error"), "bad"),
    );
  };
  const reactFromMenu = () => openPicker(document.getElementById(`msg-${m.id}`)?.getBoundingClientRect());
  const menu = [
    ...(p.canWrite ? [{ key: "react", label: t("chat.react"), icon: <IconSmile size={16} />, onSelect: reactFromMenu }] : []),
    ...(p.canWrite ? [{ key: "reply", label: t("chat.reply"), icon: <IconReply size={16} />, onSelect: p.onReply }] : []),
    { key: "forward", label: t("chat.forward"), icon: <IconTransfer size={16} />, onSelect: p.onForward },
    ...(m.body ? [{ key: "copy", label: t("chat.copyText"), icon: <IconCopy size={16} />, onSelect: copy }] : []),
    ...(p.onSaveGif ? [{ key: "gif", label: t("chat.gifSave"), icon: <IconPlus size={16} />, onSelect: p.onSaveGif }] : []),
    ...(m.editable && m.body ? [{ key: "edit", label: t("common.edit"), onSelect: p.onEdit }] : []),
    ...(p.canPin ? [{ key: "pin", label: p.pinned ? t("chat.unpinMessage") : t("chat.pinMessage"), icon: <IconPin size={16} />, onSelect: p.onPin }] : []),
    ...(m.version > 1 && p.canVersions ? [{ key: "versions", label: t("chat.versions"), onSelect: p.onVersions, separated: true }] : []),
  ];
  const meta = (chip: boolean) => (
    <span className={`inline-flex items-center gap-1 text-[11px] leading-4 whitespace-nowrap select-none ${chip ? "chat-meta-chip" : mine ? "text-white/75" : "text-muted"}`}>
      {m.version > 1 && <span>{t("chat.edited")}</span>}
      <time dateTime={m.created_at} title={formatDate(m.created_at, i18n.language)}>
        {formatTime(m.created_at, i18n.language)}
      </time>
      {mine &&
        (p.read ? (
          <>
            <IconCheckDouble size={15} className={chip || mine ? "text-white" : "text-accent"} />
            <span className="sr-only">{t("chat.read")}</span>
          </>
        ) : (
          <>
            <IconCheck size={15} />
            <span className="sr-only">{t("chat.sent")}</span>
          </>
        ))}
    </span>
  );
  // Время — в правом нижнем углу. В конце текста невидимая копия занимает его место: короткая
  // последняя строка делит его со временем, длинная — уступает ему строку ниже (как в мессенджерах).
  const metaSpace = (
    <span aria-hidden="true" className="invisible ml-2.5 inline-block">
      {meta(false)}
    </span>
  );
  const bubble = `relative min-w-0 rounded-[1.1rem] ${mine ? "chat-bubble-mine" : "chat-bubble-other"} ${tail ? (mine ? "chat-tail-out rounded-br-[4px]" : "chat-tail-in rounded-bl-[4px]") : ""}`;
  const width = images.length > 0 ? "w-[min(22rem,100%)]" : "";

  let content: ReactNode;
  if (big) {
    content = (
      <div className="flex flex-col items-end px-1">
        <span role="img" aria-label={m.body} className={`emoji leading-none ${[...m.body.trim()].length <= 2 ? "text-[4rem]" : "text-5xl"}`}>
          {m.body}
        </span>
        <span className="mt-1">{meta(true)}</span>
      </div>
    );
  } else if (bare) {
    content = (
      <div className={width}>
        <div className="overflow-hidden rounded-[1.1rem] shadow-[0_1px_2px_rgb(16_24_40/0.15)]">
          <Media files={images} overlay={meta(true)} />
        </div>
      </div>
    );
  } else {
    const head = showName || m.forwarded_from_name || m.reply;
    content = (
      <div className={`${bubble} ${width}`}>
        {head && (
          <div className="px-3 pt-1.5">
            {showName && <div className={`${who} tone-ink truncate text-[13px] font-semibold`}>{m.author_name}</div>}
            {m.forwarded_from_name && (
              <div className={`flex items-center gap-1 text-[13px] ${mine ? "text-white/85" : "text-accent"}`}>
                <IconTransfer size={12} /> {t("chat.forwardedFrom", { name: m.forwarded_from_name })}
              </div>
            )}
            {m.reply && (
              <div className={`mt-1 mb-0.5 rounded-lg py-1 pr-2 ${mine ? "bg-white/15" : "bg-accent/10"}`}>
                <Quote q={m.reply} onColor={mine} onClick={() => p.onJump(m.reply?.id ?? "")} />
              </div>
            )}
          </div>
        )}
        {images.length > 0 && (
          <div className={`p-[3px] ${head ? "pt-1.5" : ""}`}>
            <div className="overflow-hidden rounded-[0.9rem]">
              <Media files={images} overlay={!m.body && docs.length === 0 ? meta(true) : undefined} />
            </div>
          </div>
        )}
        {docs.length > 0 && (
          <div className="px-3 pt-1.5">
            <Documents files={docs} mine={mine} />
          </div>
        )}
        {m.body ? (
          <div className="relative px-3 pt-1 pb-1.5 text-[15px] leading-[1.35]">
            <MessageText body={m.body} tail={metaSpace} />
            <span className="absolute right-3 bottom-1.5">{meta(false)}</span>
          </div>
        ) : (
          (docs.length > 0 || images.length === 0) && <div className="flex justify-end px-2.5 pb-1">{meta(false)}</div>
        )}
        {m.reactions.length > 0 && (
          <div className="px-2.5 pb-2">
            <Reactions items={m.reactions} disabled={!p.canWrite} onToggle={p.onReact} mine={mine} />
          </div>
        )}
      </div>
    );
  }

  return (
    <div
      id={`msg-${m.id}`}
      className={`group/msg flex items-end gap-2 ${mine ? "justify-end" : "justify-start"} ${p.runStart ? "mt-2" : "mt-[3px]"} scroll-my-24 rounded-2xl transition-colors duration-700 ${p.flash ? "bg-accent/25" : ""} ${tail ? (mine ? "pr-2" : "pl-2") : mine ? "pr-2" : group ? "" : "pl-2"}`}
    >
      {showAvatar && <span className="w-9 shrink-0 self-end">{p.runEnd && <UserAvatar name={m.author_name} userId={m.author_id} avatar={p.avatar} size="sm" />}</span>}
      <div className={`relative flex max-w-[min(34rem,86%)] items-end gap-1 ${mine ? "flex-row-reverse" : ""}`}>
        <div className="min-w-0">
          {content}
          {(big || bare) && m.reactions.length > 0 && <Reactions items={m.reactions} disabled={!p.canWrite} onToggle={p.onReact} mine={false} />}
        </div>
        {/* Действия: на мыши — при наведении, на сенсорном экране — всегда. */}
        <div className="mb-1 flex shrink-0 items-center gap-0.5 rounded-full bg-paper/85 p-0.5 opacity-0 shadow-sm backdrop-blur transition-opacity group-hover/msg:opacity-100 focus-within:opacity-100 has-[[aria-expanded=true]]:opacity-100 [@media(hover:none)]:bg-transparent [@media(hover:none)]:opacity-100 [@media(hover:none)]:shadow-none">
          {p.canWrite && (
            <>
              <button
                ref={smile}
                type="button"
                className="rounded-full p-1.5 text-muted hover:bg-hover hover:text-ink max-sm:hidden"
                aria-label={t("chat.react")}
                aria-expanded={picking}
                onClick={(e) => (picking ? setPicking(false) : openPicker(e.currentTarget.getBoundingClientRect()))}
              >
                <IconSmile size={16} />
              </button>
              <button type="button" className="rounded-full p-1.5 text-muted hover:bg-hover hover:text-ink max-sm:hidden" aria-label={t("chat.reply")} onClick={p.onReply}>
                <IconReply size={16} />
              </button>
            </>
          )}
          <MenuButton iconOnly variant="ghost" label={t("chat.messageActions")} icon={<IconMore size={16} />} items={menu} />
        </div>
        {picking &&
          createPortal(
            <div ref={pickBox} className={`fixed z-[60] flex ${mine ? "justify-end" : "justify-start"}`} style={pickAt}>
              <ReactionPicker
                toggle={smile}
                onClose={closePicker}
                onPick={(e) => {
                  setPicking(false);
                  p.onReact(e);
                }}
              />
            </div>,
            document.body,
          )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ Поиск и файлы

function SidePanel({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  const { t } = useTranslation();
  return (
    <aside className="absolute inset-0 z-30 flex flex-col border-l border-line bg-paper sm:static sm:w-80 sm:shrink-0" aria-label={title}>
      <div className="flex items-center justify-between gap-2 border-b border-line px-3 py-2.5">
        <h3 className="font-semibold">{title}</h3>
        <button type="button" className="rounded-full p-1.5 text-muted hover:bg-hover" aria-label={t("common.close")} onClick={onClose}>
          <IconX size={18} />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
    </aside>
  );
}

function SearchPanel({ chat, onClose, onJump }: { chat: string; onClose: () => void; onJump: (id: string) => void }) {
  const { t, i18n } = useTranslation();
  const [q, setQ] = useState("");
  const [term, setTerm] = useState("");
  useEffect(() => {
    const timer = window.setTimeout(() => setTerm(q.trim()), 300);
    return () => window.clearTimeout(timer);
  }, [q]);
  const found = useQuery({
    queryKey: ["chat-search", chat, term],
    queryFn: () => ok<ChatMessage[]>(api.GET("/api/chat/{id}/search", { params: { path: { id: chat }, query: { q: term } } })),
    enabled: term.length >= 2,
  });
  return (
    <SidePanel title={t("chat.searchInChat")} onClose={onClose}>
      <div className="p-3">
        <Input type="search" autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("chat.searchPlaceholder")} aria-label={t("chat.searchInChat")} />
      </div>
      {term.length < 2 ? (
        <p className="px-3 text-sm text-muted">{t("chat.searchHint")}</p>
      ) : found.error ? (
        <ErrorBox error={found.error} />
      ) : found.isPending ? (
        <Spinner variant="list" />
      ) : found.data.length === 0 ? (
        <p className="px-3 text-sm text-muted">{t("chat.nothingFound")}</p>
      ) : (
        <>
          <p className="px-3 pb-1 text-xs text-muted">{t("chat.foundN", { count: found.data.length })}</p>
          <ul>
            {found.data.map((m) => (
              <li key={m.id}>
                <button type="button" className="flex w-full items-start gap-2.5 px-3 py-2 text-left hover:bg-hover" onClick={() => onJump(m.id)}>
                  <UserAvatar name={m.author_name} userId={m.author_id} size="sm" />
                  <span className="min-w-0 flex-1">
                    <span className="flex items-baseline justify-between gap-2">
                      <span className="truncate text-sm font-medium">{m.author_name}</span>
                      <span className="shrink-0 text-[11px] text-muted">{formatDate(m.created_at, i18n.language)}</span>
                    </span>
                    <Highlighted text={m.body} term={term} />
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </SidePanel>
  );
}

/** Фрагмент вокруг первого совпадения, совпадение подсвечено. */
function Highlighted({ text, term }: { text: string; term: string }) {
  const i = text.toLowerCase().indexOf(term.toLowerCase());
  if (i < 0) return <span className="line-clamp-2 text-sm text-muted">{text}</span>;
  const from = Math.max(0, i - 40);
  return (
    <span className="line-clamp-2 text-sm text-muted">
      {from > 0 && "…"}
      {text.slice(from, i)}
      <mark className="rounded bg-accent/25 px-0.5 text-ink">{text.slice(i, i + term.length)}</mark>
      {text.slice(i + term.length, i + term.length + 120)}
    </span>
  );
}

function FilesPanel({ chat, onClose, onJump }: { chat: string; onClose: () => void; onJump: (id: string) => void }) {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState<number | null>(null);
  const q = useQuery({ queryKey: ["chat-files", chat], queryFn: () => ok<ChatSharedFile[]>(api.GET("/api/chat/{id}/files", { params: { path: { id: chat } } })) });
  const images = (q.data ?? []).filter((x) => x.file.image && !x.file.expired);
  const docs = (q.data ?? []).filter((x) => !x.file.image || x.file.expired);
  return (
    <SidePanel title={t("chat.sharedFiles")} onClose={onClose}>
      {q.error ? (
        <ErrorBox error={q.error} />
      ) : q.isPending ? (
        <Spinner variant="list" />
      ) : q.data.length === 0 ? (
        <Empty icon={<IconFileText size={28} />}>{t("chat.noSharedFiles")}</Empty>
      ) : (
        <div className="space-y-4 p-3">
          {images.length > 0 && (
            <section>
              <h4 className="mb-1.5 text-xs font-semibold text-muted uppercase">{t("chat.photos", { count: images.length })}</h4>
              <div className="grid grid-cols-3 gap-1">
                {images.map((x, i) => (
                  <button key={x.file.id} type="button" className="relative aspect-square overflow-hidden rounded-lg bg-sunken" onClick={() => setOpen(i)} aria-label={x.file.name}>
                    <img src={chatFileUrl(x.file.id)} alt="" loading="lazy" className="size-full object-cover transition-transform hover:scale-105" />
                    {x.file.mime === "image/gif" && <span className="absolute top-1 left-1 rounded bg-black/50 px-1 text-[9px] font-bold text-white">GIF</span>}
                  </button>
                ))}
              </div>
            </section>
          )}
          {docs.length > 0 && (
            <section>
              <h4 className="mb-1.5 text-xs font-semibold text-muted uppercase">{t("chat.documents", { count: docs.length })}</h4>
              <ul className="space-y-1">
                {docs.map((x) => (
                  <li key={x.file.id} className="flex items-center gap-2.5 rounded-xl px-1.5 py-1.5 hover:bg-hover">
                    <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-accent text-paper">
                      <IconFileText size={18} />
                    </span>
                    <span className="min-w-0 flex-1">
                      {x.file.expired ? (
                        <span className="block truncate text-sm text-muted line-through">{x.file.name}</span>
                      ) : (
                        <a href={chatFileUrl(x.file.id)} download={x.file.name} className="block truncate text-sm font-medium hover:underline">
                          {x.file.name}
                        </a>
                      )}
                      <span className="block truncate text-[11px] text-muted">
                        {x.file.expired ? t("chat.expired") : formatSize(x.file.size)} · {x.author_name} · {formatDate(x.created_at, i18n.language)}
                      </span>
                    </span>
                    <button type="button" className="shrink-0 rounded-full p-1.5 text-muted hover:bg-paper hover:text-ink" aria-label={t("chat.showInChat")} title={t("chat.showInChat")} onClick={() => onJump(x.message_id)}>
                      <IconArrowLeft size={14} />
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </div>
      )}
      {open !== null && (
        <Lightbox
          items={images.map((x) => ({ src: chatFileUrl(x.file.id), name: x.file.name }))}
          index={open}
          onIndex={setOpen}
          onClose={() => setOpen(null)}
        />
      )}
    </SidePanel>
  );
}
