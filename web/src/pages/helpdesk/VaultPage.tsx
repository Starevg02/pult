// Хранилище секретов: список открытых мне секретов, карточка с показом значения, правка и доступ.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ApiError, ok, type Person, type VaultAccess, type VaultAccessList, type VaultItem, type VaultRole } from "../../api/client";
import { formatDate } from "../../format";
import { can, useMe } from "../../session";
import { Badge, Button, Card, CopyButton, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { IconPlus, IconShield } from "../../icons";
import { PersonPicker } from "./NewTicketPage";

const LEVELS = ["view", "reveal", "manage"] as const;
type Level = (typeof LEVELS)[number];
const asLevel = (v: string): Level => LEVELS.find((l) => l === v) ?? "view";
const area = "w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm";
/** Показанное значение прячется само через 30 секунд. */
const SHOW_SECONDS = 30;

function isLink(url: string) {
  return /^https?:\/\//i.test(url);
}

export function VaultPage() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["vault"], queryFn: () => ok<VaultItem[]>(api.GET("/api/helpdesk/vault")) });
  const [search, setSearch] = useState("");
  const [creating, setCreating] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const needle = search.trim().toLowerCase();
  const rows = q.data?.filter((s) => !needle || [s.name, s.login, s.url, s.description].some((v) => v.toLowerCase().includes(needle)));
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <PageHeader title={t("vault.title")} subtitle={t("vault.subtitle")}>
        <Button variant="primary" onClick={() => setCreating(true)}>
          <IconPlus size={14} />
          {t("vault.create")}
        </Button>
      </PageHeader>
      <Input value={search} onChange={(e) => setSearch(e.target.value)} placeholder={t("vault.search")} aria-label={t("vault.search")} />
      {q.error && <ErrorBox error={q.error} />}
      {q.isPending && <Spinner variant="list" />}
      {rows && rows.length === 0 && (
        <Card>
          <Empty icon={<IconShield size={20} />} hint={needle ? undefined : t("vault.emptyHint")}>
            {needle ? t("vault.notFound") : t("vault.empty")}
          </Empty>
        </Card>
      )}
      {rows && rows.length > 0 && (
        <Card>
          <ul className="divide-y divide-line text-sm">
            {rows.map((s) => (
              <li key={s.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2.5">
                <button type="button" className="min-w-0 flex-1 truncate text-left font-medium text-accent hover:underline" onClick={() => setOpen(s.id)}>
                  {s.name}
                </button>
                {s.login && (
                  <span className="inline-flex items-center gap-1 font-mono text-xs text-muted">
                    {s.login}
                    <CopyButton value={s.login} label={t("vault.copyLogin")} />
                  </span>
                )}
                <Badge tone={s.level === "manage" ? "accent" : "muted"}>{t(`vault.levels.${asLevel(s.level)}`)}</Badge>
              </li>
            ))}
          </ul>
        </Card>
      )}
      {creating && <NewSecretDialog onClose={() => setCreating(false)} onCreated={(id) => setOpen(id)} />}
      {open && <SecretDialog id={open} onClose={() => setOpen(null)} />}
    </div>
  );
}

function NewSecretDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [f, setF] = useState({ name: "", login: "", url: "", description: "" });
  const [value, setValue] = useState("");
  const create = useMutation({
    mutationFn: () => ok(api.POST("/api/helpdesk/vault", { body: { ...f, value } })),
    onSuccess: (r) => {
      setValue("");
      void qc.invalidateQueries({ queryKey: ["vault"] });
      onClose();
      onCreated(r.id);
    },
  });
  return (
    <Dialog open onClose={onClose} title={t("vault.create")}>
      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (f.name.trim() && value) create.mutate();
        }}
      >
        <Fields f={f} setF={setF} />
        <Field label={t("vault.value")} hint={t("vault.valueHint")}>
          <Input type="password" autoComplete="new-password" value={value} onChange={(e) => setValue(e.target.value)} />
        </Field>
        {create.error && <ErrorBox error={create.error} />}
        <div className="flex justify-end gap-2">
          <Button type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" variant="primary" disabled={!f.name.trim() || !value || create.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

type FieldsState = { name: string; login: string; url: string; description: string };

function Fields({ f, setF }: { f: FieldsState; setF: (f: FieldsState) => void }) {
  const { t } = useTranslation();
  return (
    <>
      <Field label={t("vault.name")}>
        <Input autoFocus value={f.name} maxLength={200} onChange={(e) => setF({ ...f, name: e.target.value })} />
      </Field>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t("vault.login")}>
          <Input value={f.login} maxLength={200} autoComplete="off" onChange={(e) => setF({ ...f, login: e.target.value })} />
        </Field>
        <Field label={t("vault.url")}>
          <Input value={f.url} maxLength={500} onChange={(e) => setF({ ...f, url: e.target.value })} placeholder="https://" />
        </Field>
      </div>
      <Field label={t("vault.description")} hint={t("vault.descriptionHint")}>
        <textarea className={area} rows={3} maxLength={2000} value={f.description} onChange={(e) => setF({ ...f, description: e.target.value })} />
      </Field>
    </>
  );
}

function SecretDialog({ id, onClose }: { id: string; onClose: () => void }) {
  const { t, i18n } = useTranslation();
  const q = useQuery({ queryKey: ["vault", id], queryFn: () => ok<VaultItem>(api.GET("/api/helpdesk/vault/{id}", { params: { path: { id } } })) });
  const s = q.data;
  return (
    <Dialog open onClose={onClose} title={s?.name ?? t("vault.title")} wide>
      {q.error && <ErrorBox error={q.error} />}
      {q.isPending && <Spinner />}
      {s && (
        <div className="space-y-4">
          <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[10rem_1fr]">
            {s.login && (
              <>
                <dt className="text-muted">{t("vault.login")}</dt>
                <dd className="flex items-center gap-1 font-mono">
                  {s.login}
                  <CopyButton value={s.login} label={t("vault.copyLogin")} />
                </dd>
              </>
            )}
            {s.url && (
              <>
                <dt className="text-muted">{t("vault.url")}</dt>
                <dd className="break-all">
                  {isLink(s.url) ? (
                    <a href={s.url} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
                      {s.url}
                    </a>
                  ) : (
                    s.url
                  )}
                </dd>
              </>
            )}
            {s.description && (
              <>
                <dt className="text-muted">{t("vault.description")}</dt>
                <dd className="whitespace-pre-wrap">{s.description}</dd>
              </>
            )}
            <dt className="text-muted">{t("vault.valueChanged")}</dt>
            <dd>{formatDate(s.value_changed_at, i18n.language)}</dd>
            {s.created_by_name && (
              <>
                <dt className="text-muted">{t("vault.createdBy")}</dt>
                <dd>{s.created_by_name}</dd>
              </>
            )}
          </dl>
          <Reveal item={s} />
          {s.level === "manage" && (
            <>
              <EditSecret item={s} onDeleted={onClose} />
              <AccessEditor id={s.id} />
            </>
          )}
        </div>
      )}
    </Dialog>
  );
}

/** Показ значения: по кнопке, с причиной для журнала, само прячется через 30 секунд. */
function Reveal({ item }: { item: VaultItem }) {
  const { t } = useTranslation();
  const me = useMe().data;
  const [reason, setReason] = useState("");
  const [value, setValue] = useState<string | null>(null);
  const [left, setLeft] = useState(0);
  const show = useMutation({
    // Значение не задерживается в кэше запросов: только в состоянии компонента, пока показано.
    gcTime: 0,
    mutationFn: () => ok(api.POST("/api/helpdesk/vault/{id}/reveal", { params: { path: { id: item.id } }, body: { reason } })),
    onSuccess: (r) => {
      setValue(r.value);
      setLeft(SHOW_SECONDS);
    },
  });
  const { reset } = show;
  const hide = () => {
    setValue(null);
    reset();
  };
  useEffect(() => {
    if (value === null) return;
    const tm = setTimeout(() => {
      if (left <= 1) {
        setValue(null);
        reset();
      } else setLeft(left - 1);
    }, 1000);
    return () => clearTimeout(tm);
  }, [value, left, reset]);
  // Уровень могли понизить, пока значение показано: оно больше не выводится и пропадёт по таймеру.
  if (item.level === "view" || !can(me, "helpdesk.vault.reveal")) {
    return <p className="rounded-lg bg-sunken px-3 py-2 text-sm text-muted">{t("vault.noReveal")}</p>;
  }
  return (
    <Card className="space-y-2 p-3">
      {value === null ? (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (!show.isPending && reason.trim().length >= 3) show.mutate();
          }}
        >
          <div className="min-w-0 flex-1">
            <Field label={t("vault.reason")} hint={t("vault.reasonHint")}>
              <Input value={reason} maxLength={500} onChange={(e) => setReason(e.target.value)} />
            </Field>
          </div>
          <Button type="submit" variant="primary" disabled={show.isPending || reason.trim().length < 3}>
            {t("vault.reveal")}
          </Button>
        </form>
      ) : (
        <div className="flex items-start gap-2">
          <pre className="min-w-0 flex-1 overflow-x-auto rounded bg-hover px-3 py-2 font-mono text-sm whitespace-pre-wrap break-all">{value}</pre>
          <CopyButton value={value} label={t("vault.copyValue")} />
          <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={hide} aria-label={t("vault.hideValue")}>
            {t("vault.hide", { n: left })}
          </Button>
        </div>
      )}
      {show.error && <ErrorBox error={show.error} />}
    </Card>
  );
}

const fieldsOf = (s: VaultItem): FieldsState => ({ name: s.name, login: s.login, url: s.url, description: s.description });

function EditSecret({ item, onDeleted }: { item: VaultItem; onDeleted: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [f, setF] = useState<FieldsState>(() => fieldsOf(item));
  // Версия, от которой начата правка: свежие данные с сервера форму не сбрасывают.
  const [base, setBase] = useState(item.version);
  const [value, setValue] = useState("");
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["vault"] });
  };
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () =>
      ok(api.PUT("/api/helpdesk/vault/{id}", { params: { path: { id: item.id } }, body: { ...f, value: value || null, version: base } })),
    onSuccess: (r) => {
      setValue("");
      setBase(r.version);
      refresh();
    },
    onError: (e) => {
      if (e instanceof ApiError && e.code === "conflict") refresh();
    },
  });
  const conflict = save.error instanceof ApiError && save.error.code === "conflict";
  const remove = useMutation({
    mutationFn: () => ok(api.DELETE("/api/helpdesk/vault/{id}", { params: { path: { id: item.id } } })),
    onSuccess: () => {
      refresh();
      onDeleted();
    },
  });
  return (
    <details className="rounded-lg border border-line p-3">
      <summary className="cursor-pointer text-sm font-medium">{t("vault.edit")}</summary>
      <div className="mt-3 space-y-3">
        <Fields f={f} setF={setF} />
        <Field label={t("vault.newValue")} hint={t("vault.newValueHint")}>
          <Input type="password" autoComplete="new-password" value={value} onChange={(e) => setValue(e.target.value)} />
        </Field>
        {conflict && (
          <div role="alert" className="rounded-lg border border-bad/30 bg-bad/5 p-3 text-sm">
            <div className="mb-2 font-medium text-bad">{t("common.conflict")}</div>
            <p className="mb-2 text-muted">{t("common.conflictText")}</p>
            <Button
              onClick={() => {
                setF(fieldsOf(item));
                setBase(item.version);
                save.reset();
              }}
            >
              {t("common.reload")}
            </Button>
          </div>
        )}
        {((save.error && !conflict) || remove.error) && <ErrorBox error={remove.error ?? save.error} />}
        <div className="flex flex-wrap justify-between gap-2">
          <Button
            variant="ghost"
            className="text-bad"
            disabled={remove.isPending}
            onClick={() => {
              if (window.confirm(t("vault.deleteConfirm", { name: item.name }))) remove.mutate();
            }}
          >
            {t("common.delete")}
          </Button>
          <Button variant="primary" onClick={() => save.mutate()} disabled={!f.name.trim() || save.isPending || conflict}>
            {t("common.save")}
          </Button>
        </div>
      </div>
    </details>
  );
}

type Row = { key: string; label: string; user_id: string | null; person_id: string | null; role: string | null; level: Level };

function toRows(list: VaultAccess[]): Row[] {
  return list.map((a) => ({
    key: a.role ? `r:${a.role}` : `u:${a.user_id ?? ""}`,
    label: a.role_name ?? a.user_name ?? "—",
    user_id: a.user_id ?? null,
    person_id: null,
    role: a.role ?? null,
    level: asLevel(a.level),
  }));
}

function AccessEditor({ id }: { id: string }) {
  const { t } = useTranslation();
  const q = useQuery({
    queryKey: ["vault", id, "access"],
    queryFn: () => ok<VaultAccessList>(api.GET("/api/helpdesk/vault/{id}/access", { params: { path: { id } } })),
  });
  return (
    <details className="rounded-lg border border-line p-3">
      <summary className="cursor-pointer text-sm font-medium">{t("vault.access")}</summary>
      <div className="mt-3 space-y-3">
        <p className="text-xs text-muted">{t("vault.accessHint")}</p>
        {q.error && <ErrorBox error={q.error} />}
        {q.isPending && <Spinner />}
        {q.data && <AccessForm id={id} data={q.data} />}
      </div>
    </details>
  );
}

/** Список доступа. Правка помнит версию, от которой начата: обновление списка с сервера её не подменяет. */
function AccessForm({ id, data }: { id: string; data: VaultAccessList }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const roles = useQuery({ queryKey: ["vault-roles"], queryFn: () => ok<VaultRole[]>(api.GET("/api/helpdesk/vault-roles")) });
  const [edit, setEdit] = useState<{ base: number; rows: Row[] } | null>(null);
  const [person, setPerson] = useState<Person | null>(null);
  const [role, setRole] = useState("");
  const list = edit?.rows ?? toRows(data.entries);
  const set = (rows: Row[]) => setEdit({ base: edit?.base ?? data.version, rows });
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: (e: { base: number; rows: Row[] }) =>
      ok(
        api.PUT("/api/helpdesk/vault/{id}/access", {
          params: { path: { id } },
          body: {
            version: e.base,
            entries: e.rows.map((r) => ({ user_id: r.user_id, person_id: r.person_id, role: r.role, level: r.level })),
          },
        }),
      ),
    onSuccess: (r) => {
      setEdit(null);
      qc.setQueryData(["vault", id, "access"], r);
    },
    onError: (e) => {
      // Список уже поменял другой управляющий: показываем свежий, правку нужно повторить.
      if (e instanceof ApiError && e.code === "conflict") {
        setEdit(null);
        void qc.invalidateQueries({ queryKey: ["vault", id, "access"] });
      }
    },
  });
  const conflict = save.error instanceof ApiError && save.error.code === "conflict";
  const add = (r: Row) => {
    if (!list.some((x) => x.key === r.key)) set([...list, r]);
  };
  return (
    <>
      <ul className="divide-y divide-line rounded-lg border border-line text-sm">
        {list.map((r) => (
          <li key={r.key} className="flex flex-wrap items-center gap-2 px-3 py-1.5">
            <span className="min-w-0 flex-1 truncate">
              {r.role && <Badge>{t("vault.role")}</Badge>} {r.label}
            </span>
            <Select
              aria-label={t("vault.levelFor", { name: r.label })}
              value={r.level}
              onChange={(e) => set(list.map((x) => (x.key === r.key ? { ...x, level: asLevel(e.target.value) } : x)))}
              className="w-44"
            >
              {LEVELS.map((l) => (
                <option key={l} value={l}>
                  {t(`vault.levels.${l}`)}
                </option>
              ))}
            </Select>
            <Button
              variant="ghost"
              className="min-h-7 px-2 text-xs"
              aria-label={t("vault.removeAccess", { name: r.label })}
              onClick={() => set(list.filter((x) => x.key !== r.key))}
            >
              {t("common.delete")}
            </Button>
          </li>
        ))}
      </ul>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t("vault.addPerson")}>
          <PersonPicker
            value={person}
            onPick={(p) => {
              setPerson(null);
              if (p) add({ key: `p:${p.id}`, label: `${p.last_name} ${p.first_name}`, user_id: null, person_id: p.id, role: null, level: "view" });
            }}
          />
        </Field>
        <Field label={t("vault.addRole")}>
          <Select
            value={role}
            onChange={(e) => {
              const r = roles.data?.find((x) => x.code === e.target.value);
              setRole("");
              if (r) add({ key: `r:${r.code}`, label: r.name, user_id: null, person_id: null, role: r.code, level: "view" });
            }}
          >
            <option value="">—</option>
            {roles.data?.map((r) => (
              <option key={r.code} value={r.code}>
                {r.name}
              </option>
            ))}
          </Select>
        </Field>
      </div>
      {save.error && <ErrorBox error={conflict ? new ApiError(409, "conflict", t("common.conflictText")) : save.error} />}
      <div className="flex justify-end gap-2">
        {edit && (
          <Button onClick={() => setEdit(null)} disabled={save.isPending}>
            {t("common.cancel")}
          </Button>
        )}
        <Button variant="primary" onClick={() => edit && save.mutate(edit)} disabled={!edit || save.isPending}>
          {t("common.save")}
        </Button>
      </div>
    </>
  );
}
