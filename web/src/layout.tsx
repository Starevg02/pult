// Оболочка: левое меню | центр; меню сворачивается на узком экране.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Outlet, useLocation, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AccessRequestView, type ChangeApprovalItem, type ComputerRow, type ExtensionLink, type Me, type MyAsset, type NotificationList, type Person, type TicketRow } from "./api/client";
import { Logo, RequireAuth } from "./auth";
import { can, useLogout, useMe, useModule, useOrg, type ModuleKey } from "./session";
import { asLang, LANGUAGES, savedLanguage, setLanguage } from "./i18n";
import { applyPrefs, changeLanguage, prefs, shownDark, START_PATHS, THEMES, useSavePrefs, type ThemeChoice } from "./prefs";
import { initials } from "./format";
import { useServerEvents } from "./pages/helpdesk/helpdeskUtil";
import { CloseButton, Dialog, Input, Toaster } from "./ui";
import {
  IconAlert,
  IconArrowLeft,
  IconBell,
  IconBook,
  IconBox,
  IconChart,
  IconCheckCircle,
  IconChevronDown,
  IconChat,
  IconColumns,
  IconContacts,
  IconHome,
  IconInbox,
  IconKey,
  IconLogout,
  IconMenu,
  IconMonitor,
  IconMoon,
  IconPlus,
  IconPlusSquare,
  IconRotate,
  IconSearch,
  IconSettings,
  IconShield,
  IconSparkles,
  IconSun,
  IconTicket,
  IconUser,
} from "./icons";
import { adminGroups, visibleItems } from "./pages/admin/adminMenu";
import { OrgWizard } from "./pages/OrgWizard";
import { fullName } from "./pages/phonebookUtil";
import { useAssist } from "./pages/helpdesk/useAssist";
import { useChatUnread } from "./pages/chat/chatUtil";

type NavItem = { to: string; key: "home" | "phonebook" | "tickets" | "catalog" | "newTicket" | "desk" | "problems" | "changes" | "vault" | "assistant" | "kb" | "assets" | "computers" | "reports" | "settings" | "access" | "approvals" | "myAssets" | "chats" };

function useNotifications() {
  return useQuery({
    queryKey: ["notifications"],
    queryFn: () => ok<NotificationList>(api.GET("/api/notifications")),
    refetchInterval: 60_000,
  });
}

/** Ctrl+K: заявки и сотрудники, инженеру ещё компьютеры домена. */
function GlobalSearch() {
  const { t } = useTranslation();
  const me = useMe().data;
  const nav = useNavigate();
  const helpdesk = useModule("helpdesk");
  const phonebook = useModule("phonebook");
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen(true);
      }
    };
    const show = () => setOpen(true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("pult:search", show);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pult:search", show);
    };
  }, []);
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 200);
    return () => clearTimeout(h);
  }, [q]);
  const tickets = useQuery({
    queryKey: ["tickets", "search", needle],
    queryFn: () => ok<TicketRow[]>(api.GET("/api/helpdesk/tickets", { params: { query: { status: "all", q: needle, limit: 8 } } })),
    enabled: open && needle.length > 1 && helpdesk,
  });
  const persons = useQuery({
    queryKey: ["global-persons", needle],
    queryFn: () => ok<Person[]>(api.GET("/api/phonebook/persons", { params: { query: { q: needle, limit: 5 } } })),
    enabled: open && needle.length > 1 && phonebook,
  });
  const computers = useQuery({
    queryKey: ["global-computers", needle],
    queryFn: () => ok<ComputerRow[]>(api.GET("/api/helpdesk/computers", { params: { query: { q: needle, limit: 5 } } })),
    enabled: open && needle.length > 1 && helpdesk && can(me, "helpdesk.ticket.work"),
  });
  const searching = tickets.isFetching || persons.isFetching || computers.isFetching;
  const found = (tickets.data?.length ?? 0) + (persons.data?.length ?? 0) + (computers.data?.length ?? 0);
  const close = () => {
    setOpen(false);
    setQ("");
  };
  return (
    <Dialog open={open} onClose={close} title={t("nav.search")}>
      <Input autoFocus type="search" aria-label={t("nav.search")} value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("helpdesk.search")} />
      {/* Итог поиска — и для экранного диктора. */}
      <p role="status" className="mt-2 text-xs text-muted">
        {needle.length > 1 && !searching && found === 0 ? t("search.nothing") : ""}
      </p>
      <ul className="mt-1 space-y-0.5 text-sm">
        {tickets.data?.map((r) => (
          <li key={r.id}>
            <button
              type="button"
              className="flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-left hover:bg-hover"
              onClick={() => {
                close();
                void nav({ to: "/tickets/$number", params: { number: String(r.number) } });
              }}
            >
              <IconTicket className="text-muted" />
              <span className="font-mono text-xs text-muted">#{r.number}</span>
              <span className="min-w-0 flex-1 truncate">{r.title}</span>
            </button>
          </li>
        ))}
        {persons.data?.map((p) => (
          <li key={p.id} className="flex items-center gap-2.5 px-2 py-2">
            <IconUser className="text-muted" />
            <span className="font-medium">{fullName(p)}</span>
            <span className="ml-auto font-mono">{p.internal_ext ?? p.work_phone ?? p.mobile_phone}</span>
          </li>
        ))}
        {computers.data?.map((c) => (
          <li key={c.id} className="flex items-center gap-2.5 px-2 py-2">
            <IconMonitor className="text-muted" />
            <span className="font-mono font-medium">{c.name}</span>
            <span className="truncate text-muted">{c.description ?? c.operating_system}</span>
            <span className="ml-auto font-mono text-muted">{c.addresses[0]}</span>
          </li>
        ))}
      </ul>
    </Dialog>
  );
}

/** Переключатель «светлая / тёмная» в шапке; «как в системе» — в меню учётки и в профиле. */
function ThemeToggle({ me }: { me: Me }) {
  const { t } = useTranslation();
  const save = useSavePrefs();
  const dark = shownDark(me.prefs.theme);
  const label = dark ? t("theme.toLight") : t("theme.toDark");
  return (
    <button type="button" aria-label={label} title={label} className={iconButton} onClick={() => save.mutate({ ...prefs(), theme: dark ? "light" : "dark" })}>
      {dark ? <IconSun size={18} /> : <IconMoon size={18} />}
    </button>
  );
}

/** Учётная запись: кто вошёл, профиль, тема, язык, выход. */
function UserMenu({ me, onLogout }: { me: Me; onLogout: () => void }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const save = useSavePrefs();
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  // Без выбора в этом браузере — язык учётки (выбранный на другом компьютере или язык установки).
  useEffect(() => {
    const own = asLang(me.locale) ?? "ru";
    if (!savedLanguage() && i18n.language !== own) {
      setLanguage(own, false);
      void qc.invalidateQueries();
    }
  }, [me.locale, i18n.language, qc]);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        trigger.current?.focus();
      }
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  return (
    <div
      ref={box}
      className="relative"
      onBlur={(e) => {
        // Фокус ушёл из меню клавиатурой (Tab) — меню закрывается.
        if (open && e.relatedTarget && !box.current?.contains(e.relatedTarget as Node)) setOpen(false);
      }}
    >
      <button
        ref={trigger}
        type="button"
        aria-expanded={open}
        aria-controls={open ? "pult-account" : undefined}
        aria-label={`${t("nav.account")}: ${me.display_name}`}
        className="flex min-h-8 items-center gap-2 rounded-lg py-1 pr-1.5 pl-1 text-sm hover:bg-hover"
        onClick={() => setOpen(!open)}
      >
        <span aria-hidden="true" className="flex h-7 w-7 items-center justify-center rounded-full bg-accentsoft text-xs font-semibold text-accent-ink">
          {initials(me.display_name)}
        </span>
        <span className="hidden max-w-40 truncate lg:inline">{me.display_name}</span>
        <IconChevronDown size={14} className={`text-muted transition-transform ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <div id="pult-account" className="animate-pop-in absolute right-0 z-40 mt-1.5 w-72 origin-top-right rounded-xl border border-line bg-paper p-2 shadow-xl">
          <div className="flex items-center gap-3 px-2 pt-1 pb-3">
            <span aria-hidden="true" className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-accentsoft text-sm font-semibold text-accent-ink">
              {initials(me.display_name)}
            </span>
            <div className="min-w-0">
              <div className="truncate font-medium">{me.display_name}</div>
              <div className="truncate font-mono text-xs text-muted">{me.login}</div>
            </div>
          </div>
          <Link to="/profile" className="flex min-h-9 items-center gap-2.5 rounded-lg px-2 text-sm hover:bg-hover" onClick={() => setOpen(false)}>
            <IconUser className="text-muted" />
            {t("nav.profile")}
          </Link>
          <div className="my-1.5 border-t border-line" />
          <label className="flex min-h-9 items-center justify-between gap-2 px-2 text-sm">
            {t("theme.label")}
            <select
              value={me.prefs.theme}
              onChange={(e) => save.mutate({ ...prefs(), theme: e.target.value as ThemeChoice })}
              className="min-h-7 rounded-md border border-control bg-paper px-1.5 text-xs"
            >
              {THEMES.map((v) => (
                <option key={v} value={v}>
                  {t(`theme.${v}`)}
                </option>
              ))}
            </select>
          </label>
          <label className="flex min-h-9 items-center justify-between gap-2 px-2 text-sm">
            {t("nav.language")}
            <select value={i18n.language} onChange={(e) => changeLanguage(asLang(e.target.value) ?? "ru", qc)} className="min-h-7 rounded-md border border-control bg-paper px-1.5 text-xs">
              {LANGUAGES.map(([code, name]) => (
                <option key={code} value={code}>
                  {name}
                </option>
              ))}
            </select>
          </label>
          <div className="my-1.5 border-t border-line" />
          <button type="button" className="flex min-h-9 w-full items-center gap-2.5 rounded-lg px-2 text-left text-sm hover:bg-hover" onClick={onLogout}>
            <IconLogout className="text-muted" />
            {t("nav.logout")}
          </button>
        </div>
      )}
    </div>
  );
}

/** Узкий экран (меню — выезжающая панель). */
function useNarrow() {
  const query = "(max-width: 767px)";
  const [narrow, setNarrow] = useState(() => window.matchMedia?.(query).matches ?? false);
  useEffect(() => {
    const m = window.matchMedia?.(query);
    if (!m) return;
    const on = () => setNarrow(m.matches);
    m.addEventListener("change", on);
    return () => m.removeEventListener("change", on);
  }, []);
  return narrow;
}

const iconButton = "relative inline-flex h-9 w-9 items-center justify-center rounded-lg text-muted hover:bg-hover hover:text-ink";

const NAV_ICONS: Record<NavItem["key"], (p: { size?: number; className?: string }) => ReactNode> = {
  home: IconHome,
  newTicket: IconPlusSquare,
  tickets: IconTicket,
  catalog: IconColumns,
  access: IconKey,
  approvals: IconCheckCircle,
  desk: IconInbox,
  problems: IconAlert,
  changes: IconRotate,
  vault: IconShield,
  assistant: IconSparkles,
  kb: IconBook,
  assets: IconBox,
  myAssets: IconBox,
  computers: IconMonitor,
  reports: IconChart,
  phonebook: IconContacts,
  chats: IconChat,
  settings: IconSettings,
};

function Shell({ me }: { me: Me }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const logout = useLogout();
  const inAdmin = useLocation().pathname.startsWith("/admin/");
  const n = useNotifications();
  const unread = n.data?.unread ?? 0;
  useServerEvents(true);
  const nav = useNavigate();
  const path = useLocation().pathname;
  // Тема и плотность из учётки (выбранные на другом компьютере) — поверх сохранённых в браузере.
  useEffect(() => applyPrefs(me.prefs), [me.prefs]);
  useEffect(() => {
    document.title = unread > 0 ? `(${unread}) Pult` : "Pult";
  }, [unread]);

  const modules = useOrg().data?.modules;
  const on = (m: ModuleKey) => modules?.[m] ?? true;
  const assist = useAssist(on("helpdesk"));
  // Стартовая страница: один раз за вкладку, если открыта главная.
  useEffect(() => {
    let started: boolean;
    try {
      started = sessionStorage.getItem("pult.started") === "1";
      sessionStorage.setItem("pult.started", "1");
    } catch {
      started = true;
    }
    const to = START_PATHS[me.prefs.start_page];
    // Раздел мог стать недоступным (сняли права, выключили модуль) — тогда остаёмся на главной.
    const ok = { home: true, tickets: on("helpdesk"), kb: on("helpdesk"), desk: on("helpdesk") && can(me, "helpdesk.ticket.work"), phonebook: on("phonebook"), notifications: true };
    if (!started && path === "/" && to !== "/" && ok[me.prefs.start_page]) void nav({ to });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- только при входе в оболочку
  }, []);
  const chatUnread = useChatUnread();
  // Ссылки установленных расширений (внешние сервисы) — в новой вкладке.
  const extLinks = useQuery({
    queryKey: ["extension-links"],
    queryFn: () => ok<ExtensionLink[]>(api.GET("/api/extensions/links")),
    enabled: on("helpdesk"),
    staleTime: 300_000,
  });
  const hasAdmin = adminGroups.some((g) => visibleItems(g.items, me, modules).length > 0);
  // «Согласования» — тем, кому есть что согласовать (владельцам ресурсов).
  const approvals = useQuery({
    queryKey: ["approvals"],
    queryFn: () => ok<AccessRequestView[]>(api.GET("/api/helpdesk/approvals")),
    enabled: !!me.person_id && on("helpdesk"),
    refetchInterval: 120_000,
  });
  // Согласования изменений — у любого пользователя, кого правило назначило согласующим.
  const changeApprovals = useQuery({
    queryKey: ["change-approvals"],
    queryFn: () => ok<ChangeApprovalItem[]>(api.GET("/api/helpdesk/change-approvals")),
    enabled: on("helpdesk"),
    refetchInterval: 120_000,
  });
  const waiting = (approvals.data?.filter((r) => r.status === "pending").length ?? 0) + (changeApprovals.data?.filter((r) => r.status === "pending").length ?? 0);
  const anyApprovals = (approvals.data?.length ?? 0) + (changeApprovals.data?.length ?? 0) > 0;
  // «Моё оборудование» — тем, за кем что-то закреплено.
  const myAssets = useQuery({
    queryKey: ["my-assets"],
    queryFn: () => ok<MyAsset[]>(api.GET("/api/helpdesk/my/assets")),
    enabled: !!me.person_id && on("helpdesk"),
    staleTime: 300_000,
  });
  // Разделы группами: каждодневное сверху (сообщения, свои заявки), затем работа ИТ-службы, справка и личное;
  // настройки — внизу. Пункты, которые появляются по условию (оборудование, согласования), — в последней группе,
  // чтобы не сдвигать остальные.
  const when = (cond: boolean, ...items: NavItem[]) => (cond ? items : []);
  const hd = on("helpdesk");
  const engineer = hd && can(me, "helpdesk.ticket.work");
  // Модули ещё не пришли: место под «Сообщения» держим, чтобы меню не прыгало.
  const chatPending = modules === undefined;
  const daily: NavItem[] = [
    { to: "/", key: "home" },
    ...when(modules?.chat === true, { to: "/chats", key: "chats" }),
    ...when(hd, { to: "/tickets", key: "tickets" }, { to: "/catalog", key: "catalog" }),
  ];
  const it: NavItem[] = [
    ...when(engineer, { to: "/desk", key: "desk" }, { to: "/problems", key: "problems" }, { to: "/changes", key: "changes" }),
    ...when(on("remote") && can(me, "remote.device.view"), { to: "/computers", key: "computers" }),
    ...when(engineer, { to: "/assets", key: "assets" }),
    ...when(hd && can(me, "helpdesk.vault.use"), { to: "/vault", key: "vault" }),
    ...when(hd && can(me, "helpdesk.reports"), { to: "/reports", key: "reports" }),
  ];
  const help: NavItem[] = [
    ...when(hd, { to: "/kb", key: "kb" }),
    ...when(hd && assist, { to: "/assistant", key: "assistant" }),
    ...when(on("phonebook"), { to: "/phonebook", key: "phonebook" }),
  ];
  const mine: NavItem[] = [
    ...when(hd, { to: "/access", key: "access" }),
    ...when((myAssets.data?.length ?? 0) > 0, { to: "/my-assets", key: "myAssets" }),
    ...when(anyApprovals, { to: "/approvals", key: "approvals" }),
  ];
  const groups: { title?: string; items: NavItem[]; skeleton?: boolean }[] = [
    { items: daily, skeleton: chatPending },
    { title: t("nav.groupIt"), items: it },
    { title: t("nav.groupHelp"), items: help },
    { title: t("nav.groupMine"), items: mine },
  ].filter((g) => g.items.length > 0);
  // Активный пункт — не только фоном: полоса акцента слева и значок цветом акцента (различимо и без цвета фона).
  const link =
    "group flex min-h-9 items-center gap-2.5 rounded-lg border-l-2 border-transparent px-2.5 py-1.5 text-sm text-muted transition-colors hover:bg-hover hover:text-ink [&.active]:border-accent [&.active]:bg-paper [&.active]:font-medium [&.active]:text-ink [&.active]:shadow-xs";
  const narrow = useNarrow();
  const drawer = narrow && open;
  const burger = useRef<HTMLButtonElement>(null);
  const drawerClose = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(false);
  // Выехавшее меню на телефоне — модальная панель: фокус внутрь, Esc закрывает, после — фокус на «☰».
  useEffect(() => {
    if (drawer) {
      wasOpen.current = true;
      drawerClose.current?.focus();
      const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
      document.addEventListener("keydown", esc);
      return () => document.removeEventListener("keydown", esc);
    }
    if (wasOpen.current) {
      wasOpen.current = false;
      burger.current?.focus();
    }
  }, [drawer]);
  const item = (i: NavItem) => {
    const Icon = NAV_ICONS[i.key];
    // «/tickets» — точное совпадение (иначе горел бы и на «/tickets/new»), но карточка заявки — тоже этот раздел.
    const ticketCard = i.to === "/tickets" && /^\/tickets\/\d+$/.test(path);
    return (
      <Link key={i.to} to={i.to} activeOptions={{ exact: i.to === "/" || i.to === "/tickets" }} className={ticketCard ? `${link} active` : link} onClick={() => setOpen(false)}>
        <span className="text-muted group-hover:text-ink group-[.active]:text-accent">
          <Icon size={18} />
        </span>
        <span className="min-w-0 flex-1 truncate">{t(`nav.${i.key}`)}</span>
        {i.key === "approvals" && waiting > 0 && <span className="rounded-full bg-accent px-1.5 text-xs font-medium text-paper tabular-nums">{waiting}</span>}
        {i.key === "chats" && chatUnread > 0 && (
          <span className="rounded-full bg-accent px-1.5 text-xs font-medium text-paper tabular-nums" aria-label={t("chat.unread", { count: chatUnread })}>
            {chatUnread}
          </span>
        )}
      </Link>
    );
  };

  return (
    <div className="flex min-h-screen">
      <a
        href="#main"
        className="sr-only z-50 rounded-lg bg-paper px-3 py-2 text-sm font-medium text-ink shadow-lg focus:not-sr-only focus:fixed focus:top-2 focus:left-2"
      >
        {t("nav.skip")}
      </a>
      <aside
        id="pult-nav"
        // Закрытое меню на узком экране не в порядке Tab и не для экранных дикторов.
        inert={narrow && !open}
        role={drawer ? "dialog" : undefined}
        aria-modal={drawer ? true : undefined}
        aria-label={drawer ? t("nav.menu") : undefined}
        className={`pult-scroll fixed inset-y-0 left-0 z-40 flex w-64 flex-col overflow-y-auto border-r border-line bg-canvas px-3 py-4 transition-transform duration-200 md:sticky md:top-0 md:z-auto md:h-screen md:w-60 md:translate-x-0 ${open ? "translate-x-0 shadow-2xl" : "-translate-x-full"}`}
      >
        <div className="mb-5 flex items-start gap-2 px-2">
          <div className="min-w-0">
            <Logo />
            <div className="text-xs text-muted">{t("app.tagline")}</div>
          </div>
          {narrow && <CloseButton ref={drawerClose} onClick={() => setOpen(false)} className="ml-auto" />}
        </div>
        {on("helpdesk") && (
          <Link
            to="/tickets/new"
            onClick={() => setOpen(false)}
            className="mb-4 flex min-h-9 items-center justify-center gap-2 rounded-lg bg-ink px-3 text-sm font-medium text-paper shadow-sm transition-opacity hover:opacity-90"
          >
            <IconPlus size={16} />
            {t("nav.newTicket")}
          </Link>
        )}
        <nav aria-label={t("nav.menu")} className="flex flex-1 flex-col">
          {groups.map((g, gi) => (
            <div key={gi} role={g.title ? "group" : undefined} aria-labelledby={g.title ? `nav-group-${gi}` : undefined} className={`space-y-0.5 ${gi > 0 ? "mt-4" : ""}`}>
              {g.title && (
                <div id={`nav-group-${gi}`} className="px-2.5 pb-1 text-xs font-medium tracking-wide text-muted uppercase">
                  {g.title}
                </div>
              )}
              {g.items.map(item)}
              {g.skeleton && <div aria-hidden="true" className="h-9 animate-pulse rounded-lg bg-hover" />}
            </div>
          ))}
          {(extLinks.data?.length ?? 0) > 0 && (
            <div className="mt-3 space-y-0.5 border-t border-line pt-3">
              {extLinks.data?.map((l) => (
                <a key={l.url} href={l.url} target="_blank" rel="noopener noreferrer" className={link} title={l.extension}>
                  <span className="min-w-0 flex-1 truncate">{l.title}</span>
                  <span className="text-xs text-muted" aria-hidden="true">
                    ↗
                  </span>
                  <span className="sr-only">{t("extensions.newTab")}</span>
                </a>
              ))}
            </div>
          )}
          {hasAdmin && <div className="mt-auto space-y-0.5 border-t border-line pt-3">{item({ to: "/admin", key: "settings" })}</div>}
        </nav>
      </aside>
      {open && <div aria-hidden="true" className="animate-fade-in fixed inset-0 z-30 bg-black/40 md:hidden" onClick={() => setOpen(false)} />}
      {/* Под открытым меню на телефоне остальная страница недоступна (модальная панель). */}
      <div className="flex min-w-0 flex-1 flex-col" inert={drawer}>
        <header className="sticky top-0 z-20 flex h-14 items-center gap-1.5 border-b border-line bg-paper/90 px-3 backdrop-blur md:gap-2 md:px-6">
          <button ref={burger} type="button" aria-label={t("nav.menu")} aria-expanded={open} aria-controls="pult-nav" className={`${iconButton} md:hidden`} onClick={() => setOpen(true)}>
            <IconMenu size={20} />
          </button>
          <span className="min-w-0 md:hidden">
            <Logo />
          </span>
          <button
            type="button"
            className="hidden h-9 w-full max-w-sm items-center gap-2 rounded-lg border border-line bg-canvas px-3 text-sm text-muted transition-colors hover:border-line-strong hover:text-ink md:flex"
            onClick={() => window.dispatchEvent(new CustomEvent("pult:search"))}
          >
            <IconSearch />
            <span className="flex-1 text-left">{t("nav.search")}</span>
            <kbd className="rounded border border-line bg-paper px-1.5 font-mono text-[11px]">Ctrl K</kbd>
          </button>
          <div className="ml-auto flex items-center gap-0.5 md:gap-1">
            {/* На телефоне — значок: Ctrl+K там нет. */}
            <button type="button" aria-label={t("nav.search")} title={t("nav.search")} className={`${iconButton} md:hidden`} onClick={() => window.dispatchEvent(new CustomEvent("pult:search"))}>
              <IconSearch size={18} />
            </button>
            <Link
              to="/notifications"
              aria-label={unread > 0 ? t("nav.notificationsUnread", { count: unread }) : t("nav.notifications")}
              title={t("nav.notifications")}
              className={`${iconButton} [&.active]:bg-hover [&.active]:text-ink`}
            >
              <IconBell size={18} />
              {unread > 0 && (
                <span aria-hidden="true" className="absolute -top-0.5 -right-0.5 min-w-4.5 rounded-full bg-accent px-1 text-center text-[11px] leading-4.5 font-semibold text-paper tabular-nums ring-2 ring-paper">
                  {unread > 99 ? "99+" : unread}
                </span>
              )}
            </Link>
            <ThemeToggle me={me} />
            <span aria-hidden="true" className="mx-1 hidden h-6 border-l border-line md:block" />
            <UserMenu me={me} onLogout={() => logout.mutate()} />
          </div>
        </header>
        <main id="main" tabIndex={-1} className="min-w-0 flex-1 px-4 py-5 focus:outline-none md:px-8 md:py-7">
          {inAdmin && (
            <Link to="/admin" className="mb-3 inline-flex items-center gap-1.5 text-sm text-accent hover:underline">
              <IconArrowLeft size={14} />
              {t("nav.settings")}
            </Link>
          )}
          {/* Смена страницы — короткое появление (без него при «уменьшить движение»). */}
          <div key={path} className="animate-page-in">
            <Outlet />
          </div>
        </main>
        <GlobalSearch />
        <Toaster />
      </div>
    </div>
  );
}

export function Layout() {
  // Ссылка из письма открывается без входа.
  if (window.location.pathname.startsWith("/ticket-response/")) return <Outlet />;
  return <RequireAuth>{(me) => <OrgSetup me={me} />}</RequireAuth>;
}

/** Суперадминистратору новой установки — мастер настройки организации, остальным — портал. */
function OrgSetup({ me }: { me: Me }) {
  const org = useOrg();
  if (me.is_superadmin && org.data && !org.data.setup_done) return <OrgWizard org={org.data} />;
  return <Shell me={me} />;
}

/** Страница без нужного разрешения — как несуществующая (по прямой ссылке, из закладки). Сервер проверяет сам. */
export function PermGate({ perm, children }: { perm: string; children: ReactNode }) {
  const me = useMe().data;
  return can(me, perm) ? <>{children}</> : <NotFound />;
}

/** Страница выключенного модуля — как несуществующая. */
export function ModuleGate({ module, children }: { module: ModuleKey; children: ReactNode }) {
  return useModule(module) ? <>{children}</> : <NotFound />;
}

export function NotFound() {
  const { t } = useTranslation();
  return (
    <div className="mx-auto max-w-xl text-center">
      <h1 className="mb-3 text-xl font-semibold">{t("notFound.title")}</h1>
      <Link to="/" className="text-accent underline">
        {t("notFound.back")}
      </Link>
    </div>
  );
}
