// Справочник: подразделения | поиск и таблица | карточка. Горячие клавиши — как в Phonebook.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type DictItem, type Me, type Person, type PhonebookConfig } from "../api/client";
import type { components } from "../api/schema";

type UserSettings = components["schemas"]["UserSettingsBody"]["settings"];
import { useMe, useModule } from "../session";
import { PersonSessions } from "./remote/RemotePages";
import { Badge, Button, Card, CloseButton, CopyButton, Dialog, Empty, ErrorBox, Input, Spinner } from "../ui";
import { IconArchive, IconColumns, IconKeyboard, IconMail, IconPhone, IconPlus, IconSearch, IconStar } from "../icons";
import { initials } from "../format";
import { CallDialog } from "./CallDialog";
import { PersonEditor } from "./PersonEditor";

import { COLUMNS, DEFAULT_COLUMNS, fullName, mailHref, type Column } from "./phonebookUtil";

type Filter = { kind: "all" } | { kind: "favorites" } | { kind: "department"; id: string };

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const h = setTimeout(() => setV(value), ms);
    return () => clearTimeout(h);
  }, [value, ms]);
  return v;
}

function useColumns() {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["my-settings"],
    queryFn: () => ok(api.GET("/api/me/settings")),
    staleTime: Infinity,
  });
  const settings = (q.data?.settings ?? {}) as { phonebook?: { columns?: string[] } };
  const saved = settings.phonebook?.columns?.filter((c): c is Column => (COLUMNS as readonly string[]).includes(c));
  const columns = saved && saved.length > 0 ? saved : DEFAULT_COLUMNS;
  const save = useMutation({
    mutationFn: (cols: Column[]) =>
      {
      const next = { ...settings, phonebook: { ...settings.phonebook, columns: cols } };
      return ok(api.PUT("/api/me/settings", { body: { settings: next as unknown as UserSettings } }));
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["my-settings"] }),
  });
  return { columns, setColumns: (c: Column[]) => save.mutate(c) };
}

/** Ширины — по содержимому (table-auto): ФИО не уже 13rem и переносится целиком (без «…»), телефоны и кабинет не переносятся,
 *  длинные должность, отдел и площадка переносятся на вторую строку; почта обрезается с подсказкой. */
const CELL: Record<Column, string> = {
  position: "min-w-28",
  department: "min-w-28",
  work: "whitespace-nowrap",
  mobile: "whitespace-nowrap",
  email: "",
  site: "min-w-24",
  room: "whitespace-nowrap",
};
/** Колонки, которые переносятся на вторую строку вместо обрезки. */
const WRAP = new Set<Column>(["position", "department", "site"]);
/** Шапка таблицы «липнет» под шапкой портала (h-14) при прокрутке длинного списка. */
const TH = "sticky top-14 z-10 border-b border-line bg-paper px-3 py-2.5 font-medium";

function textOf(p: Person, c: Column): string | undefined {
  const v = { position: p.position_name, department: p.department_name, work: p.internal_ext ?? p.work_phone, mobile: p.mobile_phone, email: p.email, site: p.site_name, room: p.room }[c];
  return v ?? undefined;
}

export function PhonebookPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const me = useMe().data as Me;
  const telephony = useModule("telephony");
  const searchRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>({ kind: "all" });
  const [archived, setArchived] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [editing, setEditing] = useState<Person | "new" | null>(null);
  const [calling, setCalling] = useState<Person | null>(null);
  const [help, setHelp] = useState(false);
  const [columnsOpen, setColumnsOpen] = useState(false);
  const { columns, setColumns } = useColumns();
  const q = useDebounced(query, 200);

  const config = useQuery({ queryKey: ["phonebook-config"], queryFn: () => ok<PhonebookConfig>(api.GET("/api/phonebook/config")) });
  const departments = useQuery({
    queryKey: ["departments"],
    queryFn: () => ok<DictItem[]>(api.GET("/api/phonebook/departments", { params: { query: {} } })),
  });
  const persons = useQuery({
    queryKey: ["persons", q, filter, archived],
    queryFn: () =>
      ok<Person[]>(
        api.GET("/api/phonebook/persons", {
          params: {
            query: {
              q: q || undefined,
              department_id: filter.kind === "department" ? filter.id : undefined,
              favorites: filter.kind === "favorites",
              archived,
            },
          },
        }),
      ),
    placeholderData: keepPreviousData,
  });
  const list = useMemo(() => persons.data ?? [], [persons.data]);
  const current = list.find((p) => p.id === selected) ?? null;
  const canEdit = !!config.data?.can_edit;
  const total = departments.data?.reduce((s, d) => s + d.persons, 0) ?? 0;

  const favorite = useMutation({
    mutationFn: (p: Person) =>
      p.is_favorite
        ? ok(api.DELETE("/api/phonebook/favorites/{id}", { params: { path: { id: p.id } } }))
        : ok(api.PUT("/api/phonebook/favorites/{id}", { params: { path: { id: p.id } } })),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["persons"] }),
  });

  // Горячие клавиши Phonebook.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const inField = e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement || e.target instanceof HTMLTextAreaElement;
      const copy = (v: string | null | undefined) => v && void navigator.clipboard?.writeText(v);
      if ((e.ctrlKey && e.key.toLowerCase() === "f") || e.key === "F6") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      } else if (e.key === "F5") {
        e.preventDefault();
        void qc.invalidateQueries({ queryKey: ["persons"] });
        void qc.invalidateQueries({ queryKey: ["departments"] });
      } else if (e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "n" && canEdit) {
        e.preventDefault();
        setEditing("new");
      } else if (e.key === "F2" && current && canEdit) {
        e.preventDefault();
        setEditing(current);
      } else if (e.ctrlKey && e.key === "Enter" && current && telephony && config.data?.call_enabled && current.internal_ext) {
        e.preventDefault();
        setCalling(current);
      } else if (e.ctrlKey && e.shiftKey && current && ["c", "m", "e"].includes(e.key.toLowerCase())) {
        e.preventDefault();
        const k = e.key.toLowerCase();
        copy(k === "c" ? current.work_phone : k === "m" ? current.mobile_phone : current.email);
      } else if (e.ctrlKey && e.key.toLowerCase() === "d" && current) {
        e.preventDefault();
        favorite.mutate(current);
      } else if (!inField && e.key === "?") {
        setHelp(true);
      } else if ((e.key === "ArrowDown" || e.key === "ArrowUp") && list.length > 0 && (!inField || e.target === searchRef.current) && !document.querySelector("dialog[open]")) {
        e.preventDefault();
        const i = list.findIndex((p) => p.id === selected);
        const next = e.key === "ArrowDown" ? Math.min(i + 1, list.length - 1) : Math.max(i - 1, 0);
        setSelected(list[next]?.id ?? null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [qc, canEdit, current, config.data, list, selected, favorite, telephony]);

  const cell = (p: Person, c: Column) => {
    switch (c) {
      case "position":
        return p.position_name;
      case "department":
        return p.department_name;
      case "work":
        return <span className="font-mono">{p.internal_ext ?? p.work_phone}</span>;
      case "mobile":
        return <span className="font-mono">{p.mobile_phone}</span>;
      case "email":
        return p.email;
      case "site":
        return p.site_name;
      case "room":
        return p.room;
    }
  };

  return (
    <div className="flex h-full flex-col">
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <h1 className="mr-2 text-xl font-semibold tracking-tight">{t("phonebook.title")}</h1>
        <div className="relative max-w-xl min-w-48 flex-1">
          <IconSearch className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
          <Input
            ref={searchRef}
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("phonebook.searchPlaceholder")}
            className="w-full pl-9"
            aria-label={t("phonebook.searchPlaceholder")}
          />
        </div>
        <Button onClick={() => setColumnsOpen(true)} className="hidden xl:inline-flex">
          <IconColumns className="text-muted" />
          {t("phonebook.columns")}
        </Button>
        {canEdit && (
          <>
            <Button variant={archived ? "primary" : "ghost"} aria-pressed={archived} onClick={() => setArchived((a) => !a)}>
              <IconArchive />
              {t("phonebook.archive")}
            </Button>
            <Button variant="primary" onClick={() => setEditing("new")}>
              <IconPlus size={14} />
              {t("phonebook.new")}
            </Button>
          </>
        )}
        <Button variant="ghost" className="w-8 px-0" onClick={() => setHelp(true)} aria-label={t("phonebook.hotkeys.title")} title={t("phonebook.hotkeys.title")}>
          <IconKeyboard size={18} />
        </Button>
      </div>

      {/* Карточка сотрудника занимает третью колонку только когда открыта — иначе таблица во всю ширину. */}
      <div className={`grid min-h-0 flex-1 gap-4 md:grid-cols-[14rem_minmax(0,1fr)] ${current ? "2xl:grid-cols-[14rem_minmax(0,1fr)_20rem]" : ""}`}>
        <nav
          className="pult-scroll hidden space-y-0.5 self-start md:sticky md:top-[4.5rem] md:block md:max-h-[calc(100vh-5.5rem)] md:overflow-y-auto"
          aria-label={t("phonebook.departments")}
        >
          <DeptButton active={filter.kind === "all"} onClick={() => setFilter({ kind: "all" })} label={t("phonebook.all")} count={total} />
          <DeptButton active={filter.kind === "favorites"} onClick={() => setFilter({ kind: "favorites" })} label={t("phonebook.favorites")} />
          <div className="my-2 border-t border-line" />
          {departments.data?.map((d) => (
            <DeptButton
              key={d.id}
              active={filter.kind === "department" && filter.id === d.id}
              onClick={() => setFilter({ kind: "department", id: d.id })}
              label={d.name}
              count={d.persons}
            />
          ))}
        </nav>

        <section className="min-w-0">
          <div className="mb-2 flex items-center gap-2 text-sm text-muted">
            {t("phonebook.found", { count: list.length })}
            {archived && <Badge tone="bad">{t("phonebook.archiveMode")}</Badge>}
          </div>
          {persons.error ? (
            <ErrorBox error={persons.error} onRetry={() => void persons.refetch()} />
          ) : persons.isPending ? (
            <Spinner variant="list" rows={10} />
          ) : list.length === 0 ? (
            <Card>
              <Empty icon={<IconSearch size={22} />} hint={t("phonebook.emptyHint")}>
                {t("phonebook.empty")}
              </Empty>
            </Card>
          ) : (
            // overflow-clip, а не hidden: иначе «липкая» шапка таблицы прилипала бы к карточке, а не к окну.
            // Много колонок — таблица может не уместиться: тогда прокрутка вбок (шапка в этом случае не липнет).
            <Card className={columns.length > 5 ? "overflow-x-auto" : "overflow-clip"}>
              <table className="hidden w-full text-sm xl:table">
                <thead className="text-left text-xs text-muted">
                  <tr>
                    <th className={TH}>{t("phonebook.col.name")}</th>
                    {columns.map((c) => (
                      <th key={c} className={`${TH} whitespace-nowrap`}>
                        {t(`phonebook.col.${c}`)}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {list.map((p) => (
                    <tr
                      key={p.id}
                      onClick={() => setSelected(p.id)}
                      onDoubleClick={() => canEdit && setEditing(p)}
                      className={`cursor-pointer border-b border-line transition-colors last:border-0 hover:bg-hover ${p.id === selected ? "bg-accentsoft shadow-[inset_3px_0_0_var(--accent)]" : ""}`}
                    >
                      {/* ФИО переносится целиком; должность и отдел — до двух строк, а не обрезаются на полуслове. */}
                      <td className="min-w-52 px-3 py-2 align-top font-medium">
                        {/* Кнопка — выбор строки с клавиатуры (Tab, Enter) открывает карточку. */}
                        <button type="button" aria-pressed={p.id === selected} onClick={() => setSelected(p.id)} className="rounded text-left">
                          {fullName(p)}
                          {p.is_favorite && (
                            <>
                              <IconStar size={12} className="ml-1 inline fill-current align-baseline text-accent" />
                              <span className="sr-only">{t("phonebook.favorites")}</span>
                            </>
                          )}
                        </button>
                      </td>
                      {columns.map((c) => (
                        <td key={c} className={`px-3 py-2 align-top text-muted ${CELL[c]}`}>
                          {WRAP.has(c) ? (
                            <span className="line-clamp-2" title={textOf(p, c)}>
                              {cell(p, c)}
                            </span>
                          ) : (
                            <span className={c === "email" ? "block max-w-52 truncate" : ""} title={textOf(p, c)}>
                              {cell(p, c)}
                            </span>
                          )}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
              {/* Телефон: список карточек вместо таблицы. */}
              <ul className="divide-y divide-line xl:hidden">
                {list.map((p) => (
                  <li key={p.id}>
                    <button type="button" onClick={() => setSelected(p.id)} className="flex w-full items-center gap-3 px-3 py-2.5 text-left hover:bg-hover">
                      <span aria-hidden="true" className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-accentsoft text-xs font-semibold text-accent-ink">
                        {initials(`${p.last_name} ${p.first_name}`)}
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block font-medium">{fullName(p)}</span>
                        <span className="block text-xs text-muted">{[p.position_name, p.department_name].filter(Boolean).join(" · ")}</span>
                      </span>
                      {(p.internal_ext ?? p.work_phone) && <span className="font-mono text-xs text-muted">{p.internal_ext ?? p.work_phone}</span>}
                    </button>
                  </li>
                ))}
              </ul>
            </Card>
          )}
        </section>

        {current && (
          <PersonCard
            person={current}
            config={config.data}
            onClose={() => setSelected(null)}
            onEdit={canEdit ? () => setEditing(current) : undefined}
            onCall={() => setCalling(current)}
            onFavorite={() => favorite.mutate(current)}
          />
        )}
      </div>

      {editing && (
        <PersonEditor
          person={editing === "new" ? null : editing}
          me={me}
          fieldSources={config.data?.field_sources ?? {}}
          departments={departments.data ?? []}
          onClose={() => setEditing(null)}
          onSaved={(p) => {
            setEditing(null);
            setSelected(p.id);
            void qc.invalidateQueries({ queryKey: ["persons"] });
            void qc.invalidateQueries({ queryKey: ["departments"] });
          }}
        />
      )}
      {calling && <CallDialog person={calling} onClose={() => setCalling(null)} />}
      <Dialog open={columnsOpen} onClose={() => setColumnsOpen(false)} title={t("phonebook.columns")}>
        <div className="space-y-2">
          {COLUMNS.map((c) => (
            <label key={c} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={columns.includes(c)}
                onChange={(e) => setColumns(e.target.checked ? COLUMNS.filter((x) => x === c || columns.includes(x)) : columns.filter((x) => x !== c))}
              />
              {t(`phonebook.col.${c}`)}
            </label>
          ))}
        </div>
        <div className="mt-4 text-right">
          <Button onClick={() => setColumnsOpen(false)}>{t("common.close")}</Button>
        </div>
      </Dialog>
      <Dialog open={help} onClose={() => setHelp(false)} title={t("phonebook.hotkeys.title")}>
        <dl className="grid grid-cols-[8rem_1fr] gap-x-4 gap-y-1.5 text-sm">
          {(["search", "refresh", "new", "edit", "call", "copy", "favorite", "help"] as const).map((k) => (
            <div key={k} className="contents">
              <dt className="font-mono text-xs text-muted">{t(`phonebook.hotkeys.keys.${k}`)}</dt>
              <dd>{t(`phonebook.hotkeys.${k}`)}</dd>
            </div>
          ))}
        </dl>
        <div className="mt-4 text-right">
          <Button onClick={() => setHelp(false)}>{t("common.close")}</Button>
        </div>
      </Dialog>
    </div>
  );
}

function DeptButton({ active, onClick, label, count }: { active: boolean; onClick: () => void; label: string; count?: number }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-current={active ? "true" : undefined}
      className={`flex min-h-8 w-full items-center gap-2 rounded-lg px-3 py-1.5 text-left text-sm transition-colors ${active ? "bg-paper font-medium text-ink shadow-xs" : "text-muted hover:bg-hover hover:text-ink"}`}
    >
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {count !== undefined && <span className="text-xs text-muted tabular-nums">{count}</span>}
    </button>
  );
}

function PersonCard({
  person: p,
  config,
  onClose,
  onEdit,
  onCall,
  onFavorite,
}: {
  person: Person;
  config?: PhonebookConfig;
  onClose: () => void;
  onEdit?: () => void;
  onCall: () => void;
  onFavorite: () => void;
}) {
  const { t } = useTranslation();
  const remote = useModule("remote");
  const telephony = useModule("telephony");
  const qc = useQueryClient();
  const archive = useMutation({
    mutationFn: () =>
      ok(
        p.is_archived
          ? api.POST("/api/phonebook/persons/{id}/restore", { params: { path: { id: p.id } }, body: { version: p.version } })
          : api.POST("/api/phonebook/persons/{id}/archive", { params: { path: { id: p.id } }, body: { version: p.version } }),
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["persons"] });
      void qc.invalidateQueries({ queryKey: ["departments"] });
      onClose();
    },
  });
  const row = (label: string, value: string | null | undefined, mono = false) =>
    value ? (
      <div>
        <div className="text-xs text-muted">{label}</div>
        <div className="flex items-center gap-1">
          <span className={mono ? "font-mono text-sm" : "text-sm"}>{value}</span>
          <CopyButton value={value} label={t("common.copy")} />
        </div>
      </div>
    ) : null;
  return (
    <aside className="animate-fade-in fixed inset-x-0 bottom-0 z-40 max-h-[80vh] overflow-y-auto rounded-t-2xl border border-line bg-paper p-5 shadow-xl md:inset-x-auto md:top-[4.5rem] md:right-4 md:bottom-4 md:w-80 md:max-h-none md:rounded-xl 2xl:sticky 2xl:max-h-[calc(100vh-5.5rem)] 2xl:w-auto 2xl:self-start 2xl:shadow-xs">
      <div className="mb-3 flex items-start gap-2">
        <span className="flex h-10 w-10 items-center justify-center rounded-full bg-accentsoft text-sm font-semibold text-accent-ink">
          {initials(`${p.last_name} ${p.first_name}`)}
        </span>
        <button
          type="button"
          onClick={onFavorite}
          className="ml-auto inline-flex h-8 w-8 items-center justify-center rounded-lg text-accent hover:bg-hover"
          aria-label={t(p.is_favorite ? "phonebook.unfavorite" : "phonebook.favorite")}
          title={t(p.is_favorite ? "phonebook.unfavorite" : "phonebook.favorite")}
        >
          <IconStar size={18} className={p.is_favorite ? "fill-current" : ""} />
        </button>
        <CloseButton onClick={onClose} />
      </div>
      <h2 className="text-lg font-semibold">{fullName(p)}</h2>
      <p className="mb-4 text-sm text-muted">{[p.position_name, p.department_name].filter(Boolean).join(" · ")}</p>
      {p.is_archived && (
        <div className="mb-3">
          <Badge tone="bad">{t("phonebook.archived")}</Badge>
        </div>
      )}
      <div className="space-y-3">
        {row(t("phonebook.card.work"), p.internal_ext ?? p.work_phone, true)}
        {p.internal_ext && p.work_phone !== p.internal_ext && row(t("phonebook.card.workFull"), p.work_phone, true)}
        {row(t("phonebook.card.mobile"), p.mobile_phone, true)}
        {row(t("phonebook.card.email"), p.email)}
        {row(t("phonebook.card.site"), p.site_name)}
        {row(t("phonebook.card.room"), p.room)}
        {p.ad_login && row(t("phonebook.card.login"), p.ad_login, true)}
      </div>
      {remote && <PersonSessions personId={p.id} />}
      <div className="mt-5 flex flex-wrap gap-2">
        {telephony && config?.call_enabled && p.internal_ext && (
          <Button variant="primary" onClick={onCall}>
            <IconPhone size={14} />
            {t("phonebook.call")}
          </Button>
        )}
        {p.mobile_phone && (
          <a href={`tel:${p.mobile_phone.replace(/[^\d+]/g, "")}`} className="inline-flex min-h-8 items-center gap-2 rounded-lg border border-line-strong px-3 py-1.5 text-sm hover:bg-hover md:hidden">
            <IconPhone size={14} className="text-muted" />
            {t("phonebook.dial")}
          </a>
        )}
        {p.email && config && (
          <a href={mailHref(config.mail_template, p.email)} target="_blank" rel="noreferrer" className="inline-flex min-h-8 items-center gap-2 rounded-lg border border-line-strong px-3 py-1.5 text-sm hover:bg-hover">
            <IconMail size={14} className="text-muted" />
            {t("phonebook.write")}
          </a>
        )}
        {onEdit && <Button onClick={onEdit}>{t("common.edit")}</Button>}
        {onEdit && (
          <Button variant="ghost" onClick={() => archive.mutate()} disabled={archive.isPending}>
            {p.is_archived ? t("common.restore") : t("common.archive")}
          </Button>
        )}
      </div>
      {archive.error && (
        <div className="mt-3">
          <ErrorBox error={archive.error} />
        </div>
      )}
    </aside>
  );
}
