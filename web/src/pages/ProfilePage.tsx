// «Мой профиль», как в GLPI: учётка и роли, карточка справочника, личная настройка интерфейса, свои сеансы.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Me, type MySession, type PasswordChanged, type Profile, type PushConfig, type PushTestResult, type Revoked, type TotpSetup, type TotpStatus, type UserPrefs } from "../api/client";
import { formatDate } from "../format";
import { asLang, LANGUAGES } from "../i18n";
import { changeLanguage, PAGE_SIZES, prefs, THEMES, useSavePrefs } from "../prefs";
import { can, useMe, useModule } from "../session";
import { Badge, Button, Card, ErrorBox, Input, PageHeader, Select, Spinner } from "../ui";
import { currentEndpoint, pushSupported, subscribeBrowser, unsubscribeBrowser } from "../push";
import { fullName } from "./phonebookUtil";
import { ColleagueProfile } from "./people/ProfileCard";
import { UserAvatar } from "./people/presence";
import { useMyCard } from "./people/profileHooks";

function Section({ title, hint, children }: { title: string; hint?: ReactNode; children: ReactNode }) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="mb-4">
      <Card className="p-4">
        <h2 id={id} className="text-base font-semibold">
          {title}
        </h2>
        {hint && <p className="mt-1 text-sm text-muted">{hint}</p>}
        <div className="mt-3">{children}</div>
      </Card>
    </section>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-1 gap-1 py-1.5 text-sm sm:grid-cols-[12rem_1fr] sm:gap-3">
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0 break-words">{children ?? "—"}</dd>
    </div>
  );
}

/** Выбор из нескольких вариантов — радиокнопками, видно всё сразу. */
function Choice<V extends string>({ label, value, options, onChange }: { label: string; value: V; options: [V, string][]; onChange: (v: V) => void }) {
  const name = useId();
  return (
    <fieldset className="py-2">
      <legend className="mb-1.5 text-sm font-medium">{label}</legend>
      <div className="flex flex-wrap gap-2">
        {options.map(([v, text]) => (
          <label
            key={v}
            className="inline-flex cursor-pointer items-center gap-2 rounded-lg border border-line px-3 py-1.5 text-sm has-[:checked]:border-accent has-[:checked]:bg-accentsoft has-[:checked]:text-accent-ink"
          >
            <input type="radio" name={name} value={v} checked={value === v} onChange={() => onChange(v)} />
            {text}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function SelectRow({ label, hint, children, value, onChange }: { label: string; hint?: string; children: ReactNode; value: string; onChange: (v: string) => void }) {
  const id = useId();
  return (
    <div className="grid grid-cols-1 items-center gap-1 py-2 sm:grid-cols-[12rem_minmax(0,18rem)_1fr] sm:gap-3">
      <label htmlFor={id} className="text-sm font-medium">
        {label}
      </label>
      <Select id={id} value={value} onChange={(e) => onChange(e.target.value)}>
        {children}
      </Select>
      {hint && <span className="text-xs text-muted">{hint}</span>}
    </div>
  );
}

/** «Chrome · Windows» из строки браузера — коротко и без версий. */
function deviceName(ua: string | null | undefined): string {
  if (!ua) return "—";
  const browser = /YaBrowser/.test(ua)
    ? "Яндекс Браузер"
    : /Edg\//.test(ua)
      ? "Edge"
      : /Firefox\//.test(ua)
        ? "Firefox"
        : /Chrome\//.test(ua)
          ? "Chrome"
          : /Safari\//.test(ua)
            ? "Safari"
            : null;
  const os = /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Mac OS X/.test(ua) ? "macOS" : /Linux/.test(ua) ? "Linux" : null;
  const s = [browser, os].filter(Boolean).join(" · ");
  return s || ua.slice(0, 60);
}

function Sessions() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["my-sessions"], queryFn: () => ok<MySession[]>(api.GET("/api/me/sessions")) });
  const [note, setNote] = useState("");
  const end = useMutation({
    mutationFn: (handle: string) => ok(api.DELETE("/api/me/sessions/{handle}", { params: { path: { handle } } })),
    onSuccess: () => {
      setNote(t("profile.sessionEnded"));
      void qc.invalidateQueries({ queryKey: ["my-sessions"] });
    },
  });
  const others = useMutation({
    mutationFn: () => ok<Revoked>(api.POST("/api/me/sessions/revoke-others")),
    onSuccess: (r) => {
      setNote(t("profile.othersEnded", { count: r.closed }));
      void qc.invalidateQueries({ queryKey: ["my-sessions"] });
    },
  });
  const list = q.data ?? [];
  const hasOthers = list.some((s) => !s.current);
  return (
    <Section title={t("profile.sessions")} hint={t("profile.sessionsHint")}>
      {q.error ? (
        <ErrorBox error={q.error} onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <Spinner />
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-line text-left text-xs text-muted">
                  <th scope="col" className="py-2 pr-3 font-medium">
                    {t("profile.device")}
                  </th>
                  <th scope="col" className="py-2 pr-3 font-medium">
                    {t("profile.ip")}
                  </th>
                  <th scope="col" className="py-2 pr-3 font-medium">
                    {t("profile.started")}
                  </th>
                  <th scope="col" className="py-2 pr-3 font-medium">
                    {t("profile.lastSeen")}
                  </th>
                  <th scope="col" className="py-2 font-medium">
                    <span className="sr-only">{t("profile.actions")}</span>
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-line">
                {list.map((s) => (
                  <tr key={s.handle}>
                    <td className="py-2 pr-3" title={s.user_agent ?? undefined}>
                      {deviceName(s.user_agent)}
                    </td>
                    <td className="py-2 pr-3 font-mono text-xs">{s.ip ?? "—"}</td>
                    <td className="py-2 pr-3 whitespace-nowrap">{formatDate(s.created_at, i18n.language)}</td>
                    <td className="py-2 pr-3 whitespace-nowrap">{formatDate(s.last_seen_at, i18n.language)}</td>
                    <td className="py-2 text-right">
                      {s.current ? (
                        <Badge tone="accent">{t("profile.current")}</Badge>
                      ) : (
                        <Button
                          variant="ghost"
                          disabled={end.isPending}
                          aria-label={`${t("profile.endSession")}: ${deviceName(s.user_agent)}, ${s.ip ?? ""}`}
                          onClick={() => end.mutate(s.handle)}
                        >
                          {t("profile.endSession")}
                        </Button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="mt-3 flex flex-wrap items-center gap-3">
            <Button variant="danger" disabled={!hasOthers || others.isPending} onClick={() => others.mutate()}>
              {t("profile.endOthers")}
            </Button>
            <span role="status" className="text-sm text-muted">
              {note}
            </span>
          </div>
          {(end.error || others.error) && (
            <div className="mt-2">
              <ErrorBox error={end.error ?? others.error} />
            </div>
          )}
        </>
      )}
    </Section>
  );
}

function Personalization({ me }: { me: Me }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const save = useSavePrefs();
  const helpdesk = useModule("helpdesk");
  const phonebook = useModule("phonebook");
  const p = me.prefs;
  const set = <K extends keyof UserPrefs>(k: K, v: UserPrefs[K]) => save.mutate({ ...prefs(), [k]: v });
  const sampleDate = new Date(2026, 11, 31, 14, 5).toISOString();
  const sample = (date_format: UserPrefs["date_format"]) => formatDate(sampleDate, i18n.language, { date_format, clock: p.clock });
  const starts: [UserPrefs["start_page"], string][] = [
    ["home", t("nav.home")],
    ...(helpdesk ? ([["tickets", t("nav.tickets")]] as [UserPrefs["start_page"], string][]) : []),
    ...(helpdesk && can(me, "helpdesk.ticket.work") ? ([["desk", t("nav.desk")]] as [UserPrefs["start_page"], string][]) : []),
    ...(phonebook ? ([["phonebook", t("nav.phonebook")]] as [UserPrefs["start_page"], string][]) : []),
    ...(helpdesk ? ([["kb", t("nav.kb")]] as [UserPrefs["start_page"], string][]) : []),
    ["notifications", t("nav.notifications")],
  ];
  return (
    <>
      <Section title={t("profile.appearance")}>
        <Choice label={t("theme.label")} value={p.theme} options={THEMES.map((v) => [v, t(`theme.${v}`)])} onChange={(v) => set("theme", v)} />
        <Choice
          label={t("profile.density")}
          value={p.density}
          options={[
            ["comfortable", t("profile.densityComfortable")],
            ["compact", t("profile.densityCompact")],
          ]}
          onChange={(v) => set("density", v)}
        />
      </Section>
      <Section title={t("profile.formats")}>
        <SelectRow label={t("nav.language")} value={i18n.language} onChange={(v) => changeLanguage(asLang(v) ?? "ru", qc)}>
          {LANGUAGES.map(([code, name]) => (
            <option key={code} value={code}>
              {name}
            </option>
          ))}
        </SelectRow>
        <SelectRow label={t("profile.dateFormat")} value={p.date_format} onChange={(v) => set("date_format", v as UserPrefs["date_format"])} hint={t("profile.sample", { value: sample(p.date_format) })}>
          {(["auto", "dmy", "ymd", "mdy"] as const).map((f) => (
            <option key={f} value={f}>
              {f === "auto" ? t("profile.dateAuto") : sample(f)}
            </option>
          ))}
        </SelectRow>
        <Choice
          label={t("profile.clock")}
          value={p.clock}
          options={[
            ["h24", t("profile.clock24")],
            ["h12", t("profile.clock12")],
          ]}
          onChange={(v) => set("clock", v)}
        />
        <Choice
          label={t("profile.nameOrder")}
          value={p.name_order}
          options={[
            ["last_first", t("profile.lastFirst")],
            ["first_last", t("profile.firstLast")],
          ]}
          onChange={(v) => set("name_order", v)}
        />
      </Section>
      <Section title={t("profile.lists")}>
        {helpdesk && (
          <SelectRow label={t("profile.pageSize")} value={String(p.page_size)} onChange={(v) => set("page_size", Number(v))}>
            {PAGE_SIZES.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </SelectRow>
        )}
        <SelectRow label={t("profile.startPage")} hint={t("profile.startPageHint")} value={p.start_page} onChange={(v) => set("start_page", v as UserPrefs["start_page"])}>
          {starts.map(([v, text]) => (
            <option key={v} value={v}>
              {text}
            </option>
          ))}
        </SelectRow>
      </Section>
      <div role="status" aria-live="polite" className="mb-4 min-h-5 text-sm">
        {save.isError ? <ErrorBox error={save.error} /> : save.isSuccess ? <span className="text-ok">{t("common.saved")}</span> : null}
      </div>
    </>
  );
}

/** Секрет по 4 знака: так его легче перепечатать в приложение. */
function grouped(secret: string): string {
  return secret.replace(/(.{4})(?=.)/g, "$1 ");
}

function CodeInput({ value, onChange, label }: { value: string; onChange: (v: string) => void; label: string }) {
  return (
    <Input
      aria-label={label}
      placeholder="123 456"
      inputMode="numeric"
      autoComplete="one-time-code"
      pattern="[0-9 ]*"
      maxLength={7}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="w-32 font-mono tracking-widest"
    />
  );
}

/** Вход из интернета: второй фактор (код из приложения). Подключается только из сети организации. */
function TwoFactor() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["my-totp"], queryFn: () => ok<TotpStatus>(api.GET("/api/me/totp")) });
  const [setup, setSetup] = useState<TotpSetup | null>(null);
  const [code, setCode] = useState("");
  const clean = code.replace(/\s/g, "");
  const done = () => {
    setSetup(null);
    setCode("");
    void qc.invalidateQueries({ queryKey: ["my-totp"] });
  };
  const start = useMutation({
    mutationFn: () => ok<TotpSetup>(api.POST("/api/me/totp/setup")),
    onSuccess: (s) => {
      setSetup(s);
      setCode("");
    },
  });
  const confirm = useMutation({
    meta: { toast: t("profile.totpOn") },
    mutationFn: () => ok(api.POST("/api/me/totp/confirm", { body: { code: clean } })),
    onSuccess: done,
  });
  const disable = useMutation({
    meta: { toast: t("profile.totpOff") },
    mutationFn: () => ok(api.POST("/api/me/totp/disable", { body: { code: clean } })),
    onSuccess: done,
  });
  const st = q.data;
  let body: ReactNode;
  if (q.error) body = <ErrorBox error={q.error} onRetry={() => void q.refetch()} />;
  else if (!st) body = <Spinner />;
  else if (st.enabled)
    body = (
      <div className="space-y-3 text-sm">
        <p className="flex flex-wrap items-center gap-2">
          <Badge tone="ok">{t("profile.totpEnabled")}</Badge>
          <span className="text-muted">{t("profile.totpSince", { date: formatDate(st.enabled_at, i18n.language) })}</span>
        </p>
        {st.can_change ? (
          <form
            className="flex flex-wrap items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              if (clean) disable.mutate();
            }}
          >
            <CodeInput value={code} onChange={setCode} label={t("profile.totpCode")} />
            <Button type="submit" disabled={disable.isPending || !clean}>
              {t("profile.totpDisable")}
            </Button>
          </form>
        ) : (
          <p className="text-muted">{t("profile.totpInsideOnly")}</p>
        )}
        {disable.error && <ErrorBox error={disable.error} />}
      </div>
    );
  else if (!st.can_change)
    body = (
      <div className="space-y-2 text-sm">
        {st.required && <p className="text-bad">{t("profile.totpRequired")}</p>}
        <p className="text-muted">{t("profile.totpInsideOnly")}</p>
      </div>
    );
  else if (setup)
    body = (
      <form
        className="space-y-3 text-sm"
        onSubmit={(e) => {
          e.preventDefault();
          if (clean) confirm.mutate();
        }}
      >
        <ol className="list-decimal space-y-2 pl-5">
          <li>{t("profile.totpStep1")}</li>
          <li>
            {t("profile.totpStep2")}
            <div className="mt-1 rounded-md bg-sunken px-2 py-1.5 font-mono text-base break-all select-all">{grouped(setup.secret)}</div>
            <a href={setup.uri} className="mt-1 inline-block text-accent hover:underline">
              {t("profile.totpOpenApp")}
            </a>
          </li>
          <li>{t("profile.totpStep3")}</li>
        </ol>
        <div className="flex flex-wrap items-center gap-2">
          <CodeInput value={code} onChange={setCode} label={t("profile.totpCode")} />
          <Button type="submit" variant="primary" disabled={confirm.isPending || !clean}>
            {t("profile.totpConfirm")}
          </Button>
          <Button onClick={() => setSetup(null)}>{t("common.cancel")}</Button>
        </div>
        {confirm.error && <ErrorBox error={confirm.error} />}
      </form>
    );
  else
    body = (
      <div className="space-y-3 text-sm">
        <p className={st.required ? "text-bad" : "text-muted"}>{t(st.required ? "profile.totpRequired" : "profile.totpOffText")}</p>
        <Button variant="primary" onClick={() => start.mutate()} disabled={start.isPending}>
          {t("profile.totpSetup")}
        </Button>
        {start.error && <ErrorBox error={start.error} />}
      </div>
    );
  return (
    <Section title={t("profile.totp")} hint={t("profile.totpHint")}>
      {body}
    </Section>
  );
}

function DevicePush() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const cfg = useQuery({ queryKey: ["my-push"], queryFn: () => ok<PushConfig>(api.GET("/api/me/push")) });
  const local = useQuery({ queryKey: ["push-endpoint"], queryFn: currentEndpoint, enabled: pushSupported() });
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["my-push"] });
    void qc.invalidateQueries({ queryKey: ["push-endpoint"] });
  };
  const on = useMutation({
    mutationFn: async () => {
      const key = cfg.data?.public_key;
      if (!key) throw new Error(t("push.unavailable"));
      let sub;
      try {
        sub = await subscribeBrowser(key);
      } catch {
        throw new Error(t("push.denied"));
      }
      await ok(api.POST("/api/me/push", { body: sub }));
    },
    onSuccess: refresh,
  });
  const off = useMutation({
    mutationFn: async () => {
      const endpoint = await unsubscribeBrowser();
      if (endpoint) await ok(api.DELETE("/api/me/push", { body: { endpoint } }));
    },
    onSuccess: refresh,
  });
  const test = useMutation({ mutationFn: () => ok<PushTestResult>(api.POST("/api/me/push/test")) });
  if (!cfg.data?.enabled) return null;
  const subscribed = !!local.data;
  return (
    <Section title={t("push.title")} hint={t("push.hint")}>
      {!pushSupported() ? (
        <p className="text-sm text-muted">{t("push.unsupported")}</p>
      ) : (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <Badge tone={subscribed ? "ok" : "muted"}>{subscribed ? t("push.on") : t("push.off")}</Badge>
          {subscribed ? (
            <>
              <Button disabled={off.isPending} onClick={() => off.mutate()}>
                {t("push.disable")}
              </Button>
              <Button variant="ghost" disabled={test.isPending} onClick={() => test.mutate()}>
                {t("push.test")}
              </Button>
            </>
          ) : (
            <Button variant="primary" disabled={on.isPending} onClick={() => on.mutate()}>
              {t("push.enable")}
            </Button>
          )}
          <span className="text-muted">{t("push.devices", { count: cfg.data.devices })}</span>
        </div>
      )}
      {test.data && (
        <p role="status" className="mt-2 text-sm">
          {t("push.testResult", { sent: test.data.sent, failed: test.data.failed })}
        </p>
      )}
      {(on.error ?? off.error ?? test.error) && <ErrorBox error={on.error ?? off.error ?? test.error} />}
    </Section>
  );
}

function PasswordForm() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [old, setOld] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const [done, setDone] = useState<PasswordChanged | null>(null);
  const change = useMutation({
    mutationFn: () => ok<PasswordChanged>(api.POST("/api/me/password", { body: { old_password: old, new_password: next } })),
    onSuccess: (r) => {
      setOld("");
      setNext("");
      setAgain("");
      setDone(r);
      void qc.invalidateQueries({ queryKey: ["my-sessions"] });
    },
  });
  const mismatch = again !== "" && again !== next;
  return (
    <Section title={t("profile.password")} hint={t("profile.passwordHint")}>
      <form
        className="grid max-w-sm gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          setDone(null);
          change.mutate();
        }}
      >
        <label className="grid gap-1 text-sm">
          {t("profile.passwordOld")}
          <Input type="password" autoComplete="current-password" value={old} onChange={(e) => setOld(e.target.value)} required />
        </label>
        <label className="grid gap-1 text-sm">
          {t("profile.passwordNew")}
          <Input type="password" autoComplete="new-password" value={next} onChange={(e) => setNext(e.target.value)} required />
        </label>
        <label className="grid gap-1 text-sm">
          {t("profile.passwordAgain")}
          <Input type="password" autoComplete="new-password" value={again} onChange={(e) => setAgain(e.target.value)} required aria-invalid={mismatch} />
        </label>
        {mismatch && <p className="text-sm text-bad">{t("profile.passwordMismatch")}</p>}
        {change.error && <ErrorBox error={change.error} />}
        {done && (
          <p role="status" className="text-sm text-ok">
            {t("profile.passwordDone", { count: done.closed_sessions })}
          </p>
        )}
        <div>
          <Button type="submit" variant="primary" disabled={!old || !next || next !== again || change.isPending}>
            {t("profile.passwordChange")}
          </Button>
        </div>
      </form>
    </Section>
  );
}

export function ProfilePage() {
  const { t, i18n } = useTranslation();
  const me = useMe().data;
  const q = useQuery({ queryKey: ["profile"], queryFn: () => ok<Profile>(api.GET("/api/me/profile")) });
  const myCard = useMyCard();
  if (!me) return <Spinner />;
  const prof = q.data;
  const person = prof?.person;
  return (
    <div className="mx-auto max-w-4xl">
      <PageHeader title={t("profile.title")} />
      <div className="mb-4 flex items-center gap-4">
        <UserAvatar name={me.display_name} userId={me.user_id} avatar={myCard.data?.avatar} size="md" />
        <div className="min-w-0">
          <div className="truncate text-lg font-semibold">{person ? fullName(person) : me.display_name}</div>
          <div className="truncate font-mono text-sm text-muted">{prof?.upn ?? me.login}</div>
        </div>
      </div>
      {q.error ? (
        <div className="mb-4">
          <ErrorBox error={q.error} onRetry={() => void q.refetch()} />
        </div>
      ) : q.isPending ? (
        <Spinner />
      ) : (
        <div className="grid gap-4 lg:grid-cols-2 lg:gap-x-4 [&>section]:mb-0">
          <Section title={t("profile.account")}>
            <dl>
              <Row label={t("profile.login")}>
                <span className="font-mono">{prof?.login}</span>
              </Row>
              <Row label={t("profile.roles")}>
                <span className="flex flex-wrap gap-1">
                  {prof?.roles.map((r) => (
                    <Badge key={`${r.code}-${r.site ?? ""}`} tone={r.code === "employee" ? "muted" : "accent"}>
                      {r.site ? `${r.name} · ${r.site}` : r.name}
                    </Badge>
                  ))}
                </span>
              </Row>
              <Row label={t("profile.lastLogin")}>{formatDate(prof?.last_login_at, i18n.language)}</Row>
              <Row label={t("profile.since")}>{formatDate(prof?.created_at, i18n.language)}</Row>
            </dl>
          </Section>
          <Section title={t("profile.card")} hint={t("profile.cardHint")}>
            {person ? (
              <dl>
                <Row label={t("profile.position")}>{person.position}</Row>
                <Row label={t("profile.department")}>{person.department}</Row>
                <Row label={t("profile.site")}>{person.site}</Row>
                <Row label={t("profile.room")}>{person.room}</Row>
                <Row label={t("profile.workPhone")}>{[person.work_phone, person.internal_ext && `${t("profile.ext")} ${person.internal_ext}`].filter(Boolean).join(", ") || null}</Row>
                <Row label={t("profile.mobile")}>{person.mobile_phone}</Row>
                <Row label={t("profile.email")}>{person.email}</Row>
              </dl>
            ) : (
              <p className="text-sm text-muted">{t("profile.noCard")}</p>
            )}
          </Section>
        </div>
      )}
      <div className="mt-4">
        <ColleagueProfile me={me} />
        <Personalization me={me} />
        {prof?.can_change_password && <PasswordForm />}
        <DevicePush />
        <TwoFactor />
        <Sessions />
      </div>
    </div>
  );
}
