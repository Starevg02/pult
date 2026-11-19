// Мои заявки и рабочее место инженера.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type BulkResult, type EngineerView, type Site, type TicketRow } from "../../api/client";
import { formatDate } from "../../format";
import { ignoreHotkey } from "../../hotkeys";
import { useMe } from "../../session";
import { Badge, Button, Card, ChipSelect, Empty, ErrorBox, Input, PageHeader, Select, Spinner } from "../../ui";
import { IconFilter, IconPlus, IconSearch, IconSort, IconTicket, IconX } from "../../icons";
import { PriorityBadge, StatusBadge, TicketLink } from "./parts";
import { dyn, PRIORITIES, STATUSES } from "./helpdeskUtil";
import { CloseCodeSelect, SlaBadge } from "./card";
import { useGroups } from "./cardUtil";

/** Заявки списка: не больше, чем выбрано в профиле («Заявок в списке»). */
function useTickets(scope: string, status: string, q: string, site: string, sort: string, sla = "") {
  const limit = useMe().data?.prefs.page_size ?? 50;
  return useQuery({
    queryKey: ["tickets", scope, status, q, site, sort, sla, limit],
    queryFn: () => ok<TicketRow[]>(api.GET("/api/helpdesk/tickets", { params: { query: { scope, status, q: q || undefined, site_id: site || undefined, sort, sla: sla || undefined, limit } } })),
    refetchInterval: 60_000,
    // Смена вкладки или фильтра не сбрасывает список в «Загрузка…».
    placeholderData: keepPreviousData,
  });
}

export function MyTicketsPage() {
  const { t, i18n } = useTranslation();
  // «Открытые» у заявителя — вместе с решёнными: их он подтверждает или открывает заново.
  const [status, setStatus] = useState("active");
  const list = useTickets("mine", status, "", "", "updated");
  return (
    <div className="mx-auto max-w-3xl">
      <PageHeader title={t("helpdesk.myTickets")}>
        <ChipSelect label={t("helpdesk.statusLabel")} active={status !== "active"} value={status} onChange={(e) => setStatus(e.target.value)}>
          <option value="active">{t("helpdesk.filterOpen")}</option>
          <option value="all">{t("helpdesk.filterAll")}</option>
        </ChipSelect>
        <Link to="/tickets/new" className="inline-flex min-h-8 items-center gap-2 rounded-lg bg-ink px-3 py-1.5 text-sm font-medium text-paper shadow-sm hover:opacity-90">
          <IconPlus size={14} />
          {t("helpdesk.create")}
        </Link>
      </PageHeader>
      {list.error ? (
        <ErrorBox error={list.error} />
      ) : list.isPending ? (
        <Spinner variant="list" />
      ) : list.data.length === 0 ? (
        <Card>
          <Empty
            icon={<IconTicket size={22} />}
            hint={t("helpdesk.emptyMineHint")}
            action={
              <Link to="/tickets/new" className="inline-flex min-h-8 items-center gap-2 rounded-lg bg-ink px-3 text-sm font-medium text-paper">
                <IconPlus size={14} />
                {t("helpdesk.create")}
              </Link>
            }
          >
            {t("helpdesk.empty")}
          </Empty>
        </Card>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {list.data.map((r) => (
              <li key={r.id}>
                <TicketLink number={r.number} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-3 text-sm hover:bg-hover">
                  <span className="font-mono text-xs text-muted">#{r.number}</span>
                  <span className={`min-w-0 flex-1 truncate ${r.unread ? "font-semibold" : ""}`}>{r.title}</span>
                  <StatusBadge status={r.status} />
                  <span className="text-xs text-muted">{formatDate(r.updated_at, i18n.language)}</span>
                </TicketLink>
              </li>
            ))}
          </ul>
        </Card>
      )}
    </div>
  );
}

const TABS = ["assigned", "unassigned", "group", "watching", "all"] as const;
const BULK = ["take", "assign", "priority", "group", "note", "resolve", "merge"] as const;

/** Массовые действия над выбранными заявками; результат — по каждой. */
function BulkBar({ numbers, onDone }: { numbers: number[]; onDone: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [action, setAction] = useState<(typeof BULK)[number]>("take");
  const [value, setValue] = useState("");
  const [comment, setComment] = useState("");
  const [code, setCode] = useState("");
  const [result, setResult] = useState<BulkResult[] | null>(null);
  const engineers = useQuery({ queryKey: ["engineers", ""], queryFn: () => ok<EngineerView[]>(api.GET("/api/helpdesk/engineers", { params: { query: {} } })), enabled: action === "assign" });
  const groups = useGroups(action === "group");
  const run = useMutation({
    mutationFn: () =>
      ok<BulkResult[]>(
        api.POST("/api/helpdesk/tickets/bulk", {
          body: {
            action,
            numbers,
            assignee_id: action === "assign" ? value || null : null,
            priority: action === "priority" ? value || null : null,
            group_id: action === "group" ? value || null : null,
            into: action === "merge" ? Number(value) || null : null,
            comment: comment || null,
            closure_code: action === "resolve" ? code : null,
          },
        }),
      ),
    onSuccess: (r) => {
      setResult(r);
      void qc.invalidateQueries({ queryKey: ["tickets"] });
      if (r.every((x) => x.ok)) onDone();
    },
  });
  const needs = action === "assign" || action === "priority" || action === "group" || action === "merge";
  const ready = (!needs || value) && (action !== "resolve" || (code && comment.trim())) && (action !== "note" || comment.trim());
  return (
    <Card className="mb-3 p-3">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="font-medium">{t("bulk.selected", { n: numbers.length })}</span>
        <Select aria-label={t("bulk.action")} className="w-auto" value={action} onChange={(e) => { setAction(e.target.value as (typeof BULK)[number]); setValue(""); setResult(null); }}>
          {BULK.map((a) => <option key={a} value={a}>{dyn(t, `bulk.actions.${a}`)}</option>)}
        </Select>
        {action === "assign" && (
          <Select aria-label={t("helpdesk.engineer")} className="w-auto" value={value} onChange={(e) => setValue(e.target.value)}>
            <option value="">{t("helpdesk.choose")}</option>
            {engineers.data?.map((e) => <option key={e.user_id} value={e.user_id}>{e.name} · {e.load}</option>)}
          </Select>
        )}
        {action === "priority" && (
          <Select aria-label={t("helpdesk.priorityLabel")} className="w-auto" value={value} onChange={(e) => setValue(e.target.value)}>
            <option value="">{t("helpdesk.choose")}</option>
            {PRIORITIES.map((p) => <option key={p} value={p}>{dyn(t, `helpdesk.priority.${p}`)}</option>)}
          </Select>
        )}
        {action === "group" && (
          <Select aria-label={t("card.group")} className="w-auto" value={value} onChange={(e) => setValue(e.target.value)}>
            <option value="">{t("helpdesk.choose")}</option>
            {groups.data?.map((g) => <option key={g.id} value={g.id}>{g.name}</option>)}
          </Select>
        )}
        {action === "merge" && <Input className="w-32" inputMode="numeric" aria-label={t("card.mergeInto")} placeholder={t("card.mergeInto")} value={value} onChange={(e) => setValue(e.target.value.replace(/\D/g, ""))} />}
        {action === "resolve" && (
          <span className="w-56">
            <CloseCodeSelect value={code} onChange={setCode} />
          </span>
        )}
        {(action === "note" || action === "resolve" || action === "assign") && (
          <Input className="max-w-sm" aria-label={action === "resolve" ? t("helpdesk.resolveComment") : t("bulk.comment")} placeholder={action === "resolve" ? t("helpdesk.resolveComment") : t("bulk.comment")} value={comment} onChange={(e) => setComment(e.target.value)} />
        )}
        <Button variant="primary" disabled={!ready || run.isPending} onClick={() => run.mutate()}>
          {t("bulk.apply")}
        </Button>
        <Button variant="ghost" onClick={onDone}>
          {t("common.cancel")}
        </Button>
      </div>
      {run.error && <ErrorBox error={run.error} />}
      {result && result.some((r) => !r.ok) && (
        <ul className="mt-2 space-y-0.5 text-xs">
          {result.filter((r) => !r.ok).map((r) => (
            <li key={r.number} className="text-bad">#{r.number}: {r.error}</li>
          ))}
        </ul>
      )}
    </Card>
  );
}

export function DeskPage() {
  const { t, i18n } = useTranslation();
  const nav = useNavigate();
  const [tab, setTab] = useState<(typeof TABS)[number]>("assigned");
  const [status, setStatus] = useState("open");
  const [site, setSite] = useState("");
  const [sort, setSort] = useState("updated");
  const [sla, setSla] = useState("");
  const [picked, setPicked] = useState<number[]>([]);
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  const [cursor, setCursor] = useState(0);
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const list = useTickets(tab, status, needle, site, sort, sla);
  const toggle = (n: number) => setPicked((p) => (p.includes(n) ? p.filter((x) => x !== n) : [...p, n]));
  const rows = list.data;
  // Массовое действие — только над заявками, которые сейчас на экране: после смены вкладки или фильтра
  // отмеченные раньше и уже невидимые заявки в него не попадают.
  const chosen = picked.filter((n) => rows?.some((r) => r.number === n));
  const position = rows?.length ? Math.min(cursor, rows.length - 1) : 0;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!rows?.length || ignoreHotkey(e)) return;
      if (e.key === "j") setCursor(Math.min(position + 1, rows.length - 1));
      if (e.key === "k") setCursor(Math.max(position - 1, 0));
      if (e.key === "Enter") void nav({ to: "/tickets/$number", params: { number: String(rows[position]?.number) } });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [rows, position, nav]);
  // Фильтры, отличные от исходных: их число — на кнопке «Сбросить».
  const active = [status !== "open", site !== "", sla !== "", needle !== ""].filter(Boolean).length;
  const reset = () => {
    setStatus("open");
    setSite("");
    setSla("");
    setQ("");
    setNeedle("");
  };
  return (
    <div>
      <PageHeader title={t("helpdesk.desk")}>
        <Link to="/tickets/new" className="inline-flex min-h-8 items-center gap-2 rounded-lg bg-ink px-3 py-1.5 text-sm font-medium text-paper shadow-sm hover:opacity-90">
          <IconPlus size={14} />
          {t("helpdesk.create")}
        </Link>
      </PageHeader>
      {/* Вкладки — сегментами, фильтры — одной панелью «чипов». */}
      <div className="mb-3 flex flex-wrap items-center gap-3">
        <div role="group" aria-label={t("helpdesk.desk")} className="flex max-w-full gap-0.5 overflow-x-auto rounded-lg bg-sunken p-0.5">
          {TABS.map((x) => (
            <button
              key={x}
              type="button"
              aria-pressed={tab === x}
              onClick={() => setTab(x)}
              className={`min-h-8 shrink-0 rounded-md px-3 text-sm font-medium whitespace-nowrap transition-colors ${tab === x ? "bg-paper text-ink shadow-sm ring-1 ring-line-strong" : "text-muted hover:text-ink"}`}
            >
              {dyn(t, `helpdesk.tab.${x}`)}
            </button>
          ))}
        </div>
        <div className="relative min-w-48 flex-1 sm:max-w-xs">
          <IconSearch className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
          <Input type="search" className="h-8 w-full py-1 pl-9" aria-label={t("helpdesk.search")} placeholder={t("helpdesk.search")} value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      </div>
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <span className="inline-flex items-center gap-1.5 text-sm text-muted">
          <IconFilter size={14} />
          {active > 0 && <span className="font-medium text-accent-ink">{t("desk.filters", { count: active })}</span>}
        </span>
        <ChipSelect label={t("helpdesk.statusLabel")} active={status !== "open"} value={status} onChange={(e) => setStatus(e.target.value)}>
          <option value="open">{t("helpdesk.filterOpen")}</option>
          <option value="all">{t("helpdesk.filterAll")}</option>
          {STATUSES.map((s) => (
            <option key={s} value={s}>
              {dyn(t, `helpdesk.status.${s}`)}
            </option>
          ))}
        </ChipSelect>
        <ChipSelect label={t("helpdesk.site")} active={site !== ""} value={site} onChange={(e) => setSite(e.target.value)}>
          <option value="">{t("helpdesk.allSites")}</option>
          {sites.data?.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </ChipSelect>
        <ChipSelect label="SLA" active={sla !== ""} value={sla} onChange={(e) => setSla(e.target.value)}>
          <option value="">{t("bulk.slaAny")}</option>
          <option value="warning">{t("card.sla.warning")}</option>
          <option value="breached">{t("card.sla.breached")}</option>
        </ChipSelect>
        {active > 0 && (
          <Button variant="ghost" className="min-h-8 px-2" onClick={reset}>
            <IconX size={14} />
            {t("common.resetFilters")}
          </Button>
        )}
        <ChipSelect label={t("helpdesk.sortLabel")} icon={<IconSort size={14} className="text-muted" />} className="ml-auto" value={sort} onChange={(e) => setSort(e.target.value)}>
          <option value="updated">{t("helpdesk.sort.updated")}</option>
          <option value="created">{t("helpdesk.sort.created")}</option>
          <option value="priority">{t("helpdesk.sort.priority")}</option>
          <option value="due">{t("bulk.sortDue")}</option>
        </ChipSelect>
      </div>
      {chosen.length > 0 && <BulkBar numbers={chosen} onDone={() => setPicked([])} />}
      {list.error ? (
        <ErrorBox error={list.error} />
      ) : list.isPending ? (
        <Spinner variant="list" />
      ) : list.data.length === 0 ? (
        <Card>
          {active > 0 ? (
            <Empty icon={<IconFilter size={22} />} hint={t("desk.emptyFilteredHint")} action={<Button onClick={reset}>{t("common.resetFilters")}</Button>}>
              {t("desk.emptyFiltered")}
            </Empty>
          ) : (
            <Empty hint={t("desk.emptyHint")}>{t("helpdesk.empty")}</Empty>
          )}
        </Card>
      ) : (
        <Card className="overflow-hidden">
          <ul className="divide-y divide-line">
            {list.data.map((r, i) => (
              <li key={r.id} className={`flex items-stretch transition-colors ${i === position ? "border-l-2 border-accent bg-hover" : "border-l-2 border-transparent"}`}>
                {/* Флажок в метке с полями: цель для пальца и мыши не меньше 24×24. */}
                <label className="flex cursor-pointer items-center pr-1 pl-3">
                  <input type="checkbox" className="h-4 w-4" aria-label={`#${r.number}`} checked={picked.includes(r.number)} onChange={() => toggle(r.number)} />
                </label>
                <TicketLink number={r.number} className="flex min-h-11 min-w-0 flex-1 flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-sm hover:bg-hover">
                  <span className="font-mono text-xs text-muted">#{r.number}</span>
                  <span className={`min-w-40 flex-1 truncate ${r.unread ? "font-semibold" : ""}`}>
                    {r.is_major && <Badge tone="bad">{t("helpdesk.major")}</Badge>} {r.title}
                  </span>
                  {r.unverified && <Badge tone="bad">{t("helpdesk.unverified")}</Badge>}
                  <span className="hidden w-36 truncate text-muted md:inline">{r.requester_name}</span>
                  <span className="hidden w-24 truncate text-muted lg:inline">{r.site_name}</span>
                  <span className="hidden w-32 truncate text-muted xl:inline">{r.category_name}</span>
                  <PriorityBadge priority={r.priority} />
                  <StatusBadge status={r.status} />
                  <SlaBadge state={r.sla_state === "ok" || r.sla_state === "met" ? null : r.sla_state} />
                  <span className="hidden w-28 truncate text-muted lg:inline" title={r.group_name ?? undefined}>{r.assignee_name ?? r.group_name ?? "—"}</span>
                  <span className="text-xs text-muted">{formatDate(r.updated_at, i18n.language)}</span>
                </TicketLink>
              </li>
            ))}
          </ul>
        </Card>
      )}
    </div>
  );
}
