// Главная сотрудника: объявления, быстрые действия, мои открытые заявки, поиск по справочнику;
// инженеру — ещё сводка по рабочему месту.
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Person, type TicketRow } from "../api/client";
import { formatDate } from "../format";
import { IconArrowRight, IconBook, IconContacts, IconKey, IconPlus, IconSearch, IconTicket } from "../icons";
import { Announcements, StatusBadge, TicketLink } from "./helpdesk/parts";
import { can, useMe, useModule } from "../session";
import { Card, Empty, ErrorBox, Input, Skeleton, Spinner } from "../ui";
import { fullName } from "./phonebookUtil";
import { ServiceStatusCard } from "./assets/ServiceStatus";

function QuickTile({ to, icon, title, hint, primary, hue = 2 }: { to: "/tickets/new" | "/phonebook" | "/kb" | "/access"; icon: ReactNode; title: string; hint: string; primary?: boolean; hue?: number }) {
  return (
    <Link
      to={to}
      className={`group flex min-h-24 flex-col gap-3 rounded-2xl border p-4 transition-[box-shadow,border-color,transform] hover:-translate-y-0.5 hover:shadow-lg ${
        primary ? "border-transparent bg-[linear-gradient(135deg,#7b6dff,#4f46e5_60%,#4338ca)] text-white shadow-md shadow-accent/25" : "border-line bg-paper shadow-xs hover:border-line-strong"
      }`}
    >
      <span aria-hidden="true" className={`flex h-10 w-10 items-center justify-center rounded-xl transition-transform group-hover:scale-105 ${primary ? "bg-white/20" : `tone-${hue} tone-avatar shadow-sm`}`}>
        {icon}
      </span>
      <span>
        <span className="block text-sm font-semibold">{title}</span>
        <span className={`mt-0.5 block text-xs ${primary ? "text-white/80" : "text-muted"}`}>{hint}</span>
      </span>
    </Link>
  );
}

/** Сводка инженера: сколько назначено, без исполнителя и просрочено. */
function WorkCard() {
  const { t } = useTranslation();
  const mine = useQuery({
    queryKey: ["tickets", "home-work", "assigned"],
    queryFn: () => ok<TicketRow[]>(api.GET("/api/helpdesk/tickets", { params: { query: { scope: "assigned", status: "open", limit: 100 } } })),
    refetchInterval: 60_000,
  });
  const free = useQuery({
    queryKey: ["tickets", "home-work", "unassigned"],
    queryFn: () => ok<TicketRow[]>(api.GET("/api/helpdesk/tickets", { params: { query: { scope: "unassigned", status: "open", limit: 100 } } })),
    refetchInterval: 60_000,
  });
  const count = (rows?: TicketRow[]) => (rows ? (rows.length >= 100 ? "100+" : String(rows.length)) : null);
  const stats: [string, string | null, boolean][] = [
    [t("home.assignedToMe"), count(mine.data), false],
    [t("home.unassigned"), count(free.data), false],
    [t("home.breached"), count(mine.data?.filter((r) => r.sla_state === "breached")), true],
  ];
  return (
    <Card className="p-4">
      <h2 className="mb-3 text-sm font-semibold">{t("home.work")}</h2>
      <dl className="grid grid-cols-3 gap-2">
        {stats.map(([label, value, bad]) => (
          <div key={label} className="rounded-lg bg-sunken px-3 py-2.5">
            <dt className="truncate text-xs text-muted" title={label}>
              {label}
            </dt>
            <dd className={`mt-0.5 text-xl font-semibold tabular-nums ${bad && value !== "0" ? "text-bad" : ""}`}>{value ?? <Skeleton className="mt-1 h-5 w-8" />}</dd>
          </div>
        ))}
      </dl>
      <Link to="/desk" className="mt-3 inline-flex items-center gap-1.5 text-sm font-medium text-accent hover:underline">
        {t("home.openDesk")}
        <IconArrowRight size={14} />
      </Link>
    </Card>
  );
}

/** Сколько своих заявок показывать на главной. */
const HOME_LIMIT = 6;

export function HomePage() {
  const { t, i18n } = useTranslation();
  const me = useMe().data;
  const helpdesk = useModule("helpdesk");
  const phonebook = useModule("phonebook");
  const [q, setQ] = useState("");
  const [debounced, setDebounced] = useState("");
  useEffect(() => {
    const h = setTimeout(() => setDebounced(q.trim()), 200);
    return () => clearTimeout(h);
  }, [q]);
  const recent = useQuery({
    queryKey: ["tickets", "home"],
    queryFn: () => ok<TicketRow[]>(api.GET("/api/helpdesk/tickets", { params: { query: { scope: "mine", status: "active", limit: HOME_LIMIT + 1 } } })),
    enabled: helpdesk,
  });
  const found = useQuery({
    queryKey: ["home-search", debounced],
    queryFn: () => ok<Person[]>(api.GET("/api/phonebook/persons", { params: { query: { q: debounced, limit: 8 } } })),
    enabled: phonebook && debounced.length > 0,
  });
  const engineer = helpdesk && can(me, "helpdesk.ticket.work");
  // На телефоне поле поиска не забирает фокус: иначе сразу выезжает клавиатура.
  const wide = typeof window !== "undefined" && (window.matchMedia?.("(min-width: 768px)").matches ?? true);
  return (
    <div className="mx-auto max-w-6xl">
      <div className="mb-6">
        <h1 className="text-2xl font-semibold tracking-tight">{t("home.greeting", { name: me?.display_name ?? "" })}</h1>
        <p className="mt-1 text-sm text-muted">{t("home.subtitle")}</p>
      </div>
      {helpdesk && <Announcements />}
      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="min-w-0 space-y-5">
          <section aria-labelledby="home-quick">
            <h2 id="home-quick" className="sr-only">
              {t("home.quick")}
            </h2>
            <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
              {helpdesk && <QuickTile primary to="/tickets/new" icon={<IconPlus size={18} />} title={t("helpdesk.create")} hint={t("home.createHint")} />}
              {phonebook && <QuickTile hue={5} to="/phonebook" icon={<IconContacts size={18} />} title={t("nav.phonebook")} hint={t("home.phonebookHint")} />}
              {helpdesk && <QuickTile hue={1} to="/kb" icon={<IconBook size={18} />} title={t("nav.kb")} hint={t("home.kbHint")} />}
              {helpdesk && <QuickTile hue={3} to="/access" icon={<IconKey size={18} />} title={t("nav.access")} hint={t("home.accessHint")} />}
            </div>
          </section>
          {helpdesk && (
            <Card>
              <div className="flex items-center gap-3 border-b border-line px-4 py-3">
                <IconTicket className="text-muted" />
                <h2 className="text-sm font-semibold">{t("home.myOpen")}</h2>
                {recent.data && recent.data.length > 0 && (
                  <span className="rounded-full bg-sunken px-2 text-xs font-medium text-muted tabular-nums">{recent.data.length > HOME_LIMIT ? `${HOME_LIMIT}+` : recent.data.length}</span>
                )}
                <Link to="/tickets" className="ml-auto inline-flex items-center gap-1 text-sm text-accent hover:underline">
                  {t("home.allMine")}
                  <IconArrowRight size={14} />
                </Link>
              </div>
              {recent.error ? (
                <div className="p-4">
                  <ErrorBox error={recent.error} onRetry={() => void recent.refetch()} />
                </div>
              ) : recent.isPending ? (
                <div className="p-4">
                  <Spinner />
                </div>
              ) : !recent.data?.length ? (
                <Empty
                  icon={<IconTicket size={22} />}
                  hint={t("home.noOpenHint")}
                  action={
                    <Link to="/tickets/new" className="inline-flex min-h-8 items-center gap-2 rounded-lg bg-ink px-3 text-sm font-medium text-paper">
                      <IconPlus size={14} />
                      {t("helpdesk.create")}
                    </Link>
                  }
                >
                  {t("home.noOpen")}
                </Empty>
              ) : (
                <ul className="divide-y divide-line">
                  {recent.data.slice(0, HOME_LIMIT).map((r) => (
                    <li key={r.id}>
                      <TicketLink number={r.number} className="flex items-center gap-3 px-4 py-3 text-sm transition-colors hover:bg-hover">
                        <span className="w-12 shrink-0 font-mono text-xs text-muted">#{r.number}</span>
                        <span className={`min-w-0 flex-1 truncate ${r.unread ? "font-semibold" : ""}`}>{r.title}</span>
                        <span className="hidden text-xs text-muted sm:inline">{formatDate(r.updated_at, i18n.language)}</span>
                        <StatusBadge status={r.status} />
                      </TicketLink>
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          )}
        </div>
        <div className="min-w-0 space-y-5">
          {phonebook && (
            <Card className="p-4">
              <label htmlFor="home-search" className="mb-2 block text-sm font-semibold">
                {t("home.searchTitle")}
              </label>
              <div className="relative">
                <IconSearch className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
                <Input id="home-search" autoFocus={wide} type="search" className="w-full pl-9" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("phonebook.searchPlaceholder")} />
              </div>
              {found.data && (
                <ul className="mt-3 divide-y divide-line">
                  {found.data.map((p) => (
                    <li key={p.id} className="py-2 text-sm">
                      <div className="flex items-baseline gap-2">
                        <span className="min-w-0 flex-1 truncate font-medium">{fullName(p)}</span>
                        <span className="font-mono text-xs">{p.internal_ext ?? p.work_phone ?? p.mobile_phone}</span>
                      </div>
                      <div className="truncate text-xs text-muted">{[p.position_name, p.department_name].filter(Boolean).join(" · ")}</div>
                    </li>
                  ))}
                  {found.data.length === 0 && <li className="py-2 text-sm text-muted">{t("phonebook.empty")}</li>}
                </ul>
              )}
              <Link to="/phonebook" className="mt-3 inline-flex items-center gap-1 text-sm text-accent hover:underline">
                {t("home.openPhonebook")}
                <IconArrowRight size={14} />
              </Link>
            </Card>
          )}
          {engineer && <WorkCard />}
          {engineer && <ServiceStatusCard />}
        </div>
      </div>
    </div>
  );
}
