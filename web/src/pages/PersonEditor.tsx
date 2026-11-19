// Новый сотрудник и правка: предупреждение о дублях (не запрет), конфликт версий — 409
// с текущими значениями, поля из AD у связанного сотрудника — только чтение.
import { useMutation, useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ApiError, ok, type DictItem, type Duplicate, type FieldSource, type Me, type Person, type PersonInput, type Site } from "../api/client";
import { Button, Dialog, ErrorBox, Field, Input, Select } from "../ui";

type Key = "last_name" | "first_name" | "middle_name" | "work_phone" | "mobile_phone" | "email" | "room";

function toInput(p: Person | null): PersonInput {
  return {
    last_name: p?.last_name ?? "",
    first_name: p?.first_name ?? "",
    middle_name: p?.middle_name ?? null,
    department_id: p?.department_id ?? null,
    position_id: p?.position_id ?? null,
    work_phone: p?.work_phone ?? null,
    mobile_phone: p?.mobile_phone ?? null,
    email: p?.email ?? null,
    site_id: p?.site_id ?? null,
    room: p?.room ?? null,
  };
}

export function PersonEditor({
  person,
  fieldSources,
  departments,
  onClose,
  onSaved,
}: {
  person: Person | null;
  me: Me;
  fieldSources: Record<string, FieldSource>;
  departments: DictItem[];
  onClose: () => void;
  onSaved: (p: Person) => void;
}) {
  const { t } = useTranslation();
  const [v, setV] = useState<PersonInput>(toInput(person));
  const [version, setVersion] = useState(person?.version ?? 0);
  const [dups, setDups] = useState<Duplicate[] | null>(null);
  const [conflict, setConflict] = useState<Person | null>(null);
  const positions = useQuery({ queryKey: ["positions"], queryFn: () => ok<DictItem[]>(api.GET("/api/phonebook/positions", { params: { query: {} } })) });
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const locked = (field: string) => !!person?.ad_login && fieldSources[field] === "ad";

  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () =>
      person
        ? ok<Person>(api.PUT("/api/phonebook/persons/{id}", { params: { path: { id: person.id } }, body: { version, person: v } }))
        : ok<Person>(api.POST("/api/phonebook/persons", { body: { person: v } })),
    onSuccess: onSaved,
    onError: (e) => {
      if (e instanceof ApiError && e.code === "conflict" && e.details) setConflict(e.details as Person);
    },
  });
  const check = useMutation({
    mutationFn: () => ok<Duplicate[]>(api.POST("/api/phonebook/duplicates", { body: { person: v, exclude_id: person?.id ?? null } })),
    onSuccess: (d) => (d.length > 0 ? setDups(d) : save.mutate()),
  });

  const set = (k: Key) => (e: { target: { value: string } }) => setV((s) => ({ ...s, [k]: e.target.value || (k === "last_name" || k === "first_name" ? "" : null) }));
  const text = (k: Key, field = k as string) => (
    <Field label={t(`person.${k}`)} hint={locked(field) ? t("person.fromAd") : undefined}>
      <Input value={(v[k] as string | null) ?? ""} onChange={set(k)} disabled={locked(field)} />
    </Field>
  );
  const submit = (e: FormEvent) => {
    e.preventDefault();
    setDups(null);
    check.mutate();
  };

  return (
    <Dialog open onClose={onClose} title={person ? t("person.editTitle") : t("person.newTitle")} wide>
      <form onSubmit={submit} className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-3">
          {text("last_name")}
          {text("first_name")}
          {text("middle_name")}
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("person.department")}>
            <Select value={v.department_id ?? ""} onChange={(e) => setV({ ...v, department_id: e.target.value || null })} disabled={locked("department")}>
              <option value="">—</option>
              {departments.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name}
                </option>
              ))}
            </Select>
          </Field>
          <Field label={t("person.position")}>
            <Select value={v.position_id ?? ""} onChange={(e) => setV({ ...v, position_id: e.target.value || null })} disabled={locked("position")}>
              <option value="">—</option>
              {positions.data?.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name}
                </option>
              ))}
            </Select>
          </Field>
        </div>
        <div className="grid gap-3 sm:grid-cols-3">
          {text("work_phone")}
          {text("mobile_phone")}
          {text("email")}
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("person.site")}>
            <Select value={v.site_id ?? ""} onChange={(e) => setV({ ...v, site_id: e.target.value || null })} disabled={locked("site")}>
              <option value="">—</option>
              {sites.data
                ?.filter((s) => s.is_active || s.id === v.site_id)
                .map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
            </Select>
          </Field>
          {text("room")}
        </div>

        {dups && (
          <div role="alert" className="rounded-lg border border-accent/30 bg-accentsoft p-3 text-sm">
            <div className="mb-1 font-medium">{t("person.duplicates")}</div>
            <ul className="mb-2 list-disc pl-5">
              {dups.map((d) => (
                <li key={d.id}>
                  {d.name}
                  {d.department_name && ` · ${d.department_name}`} — {t(`person.dupReason.${d.reason as "name" | "work_phone" | "mobile_phone"}`)}
                </li>
              ))}
            </ul>
            <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
              {t("person.saveAnyway")}
            </Button>
          </div>
        )}
        {conflict && (
          <div role="alert" className="rounded-lg border border-bad/30 bg-bad/5 p-3 text-sm">
            <div className="mb-2 font-medium text-bad">{t("common.conflict")}</div>
            <p className="mb-2 text-muted">{t("common.conflictText")}</p>
            <Button
              onClick={() => {
                setV(toInput(conflict));
                setVersion(conflict.version);
                setConflict(null);
                save.reset();
              }}
            >
              {t("common.reload")}
            </Button>
          </div>
        )}
        {save.error && !conflict && <ErrorBox error={save.error} />}
        {check.error && <ErrorBox error={check.error} />}
        <div className="flex justify-end gap-2 border-t border-line pt-4">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button type="submit" variant="primary" disabled={save.isPending || check.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
