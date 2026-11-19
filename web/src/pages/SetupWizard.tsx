// Мастер первого запуска: код из журнала сервера → домен с проверкой → вход администратора.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ApiError, ok, type DcCheckView, type SetupDomain, type SetupStatus } from "../api/client";
import { LanguagePicker, Logo } from "../auth";
import { Badge, Button, ErrorBox, Field, Input, Select } from "../ui";

const textarea = "w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs focus:border-accent";

function CodeStep({ onDone }: { onDone: (code: string) => void }) {
  const { t } = useTranslation();
  const [code, setCode] = useState("");
  const check = useMutation({
    mutationFn: () => ok(api.POST("/api/setup/verify", { headers: { "X-Setup-Token": code.trim() } })),
    onSuccess: () => onDone(code.trim()),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (code.trim()) check.mutate();
  };
  return (
    <form onSubmit={submit} className="space-y-4">
      <p className="text-sm text-muted">{t("setup.codeText")}</p>
      <div className="text-xs text-muted">{t("setup.codeDeb")}</div>
      <pre className="whitespace-pre-wrap break-all rounded-lg bg-hover px-3 py-2 font-mono text-xs">sudo journalctl -u pult | grep "SETUP CODE" | tail -n 1</pre>
      <div className="text-xs text-muted">{t("setup.codeDocker")}</div>
      <pre className="whitespace-pre-wrap break-all rounded-lg bg-hover px-3 py-2 font-mono text-xs">docker compose -f deploy/compose.yml logs pult | grep "SETUP CODE" | tail -n 1</pre>
      <Field label={t("setup.code")}>
        <Input autoFocus autoComplete="off" className="font-mono" value={code} onChange={(e) => setCode(e.target.value)} />
      </Field>
      {check.error instanceof ApiError && check.error.status === 403 ? (
        <div role="alert" className="rounded-lg bg-bad/5 px-3 py-2 text-sm text-bad">
          {t("setup.badCode")}
        </div>
      ) : (
        check.error && <ErrorBox error={check.error} />
      )}
      <Button type="submit" variant="primary" className="w-full py-2.5" disabled={check.isPending || !code.trim()}>
        {t("setup.next")}
      </Button>
    </form>
  );
}

function DomainStep({ code, onDone }: { code: string; onDone: (d: SetupDomain) => void }) {
  const { t } = useTranslation();
  const [dns, setDns] = useState("");
  const [controllers, setControllers] = useState("");
  const [tls, setTls] = useState("ldaps");
  const [bindDn, setBindDn] = useState("");
  const [password, setPassword] = useState("");
  const [ca, setCa] = useState("");
  const [checks, setChecks] = useState<DcCheckView[] | null>(null);
  const domain = (): SetupDomain => ({
    dns_name: dns.trim(),
    controllers: controllers.split(/[\s,]+/).filter(Boolean),
    tls_mode: tls,
    bind_dn: bindDn.trim(),
    bind_password: password,
    ca_cert_pem: ca.trim() || null,
  });
  const test = useMutation({
    mutationFn: () => ok<DcCheckView[]>(api.POST("/api/setup/test", { headers: { "X-Setup-Token": code }, body: domain() })),
    onMutate: () => setChecks(null),
    onSuccess: setChecks,
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    test.mutate();
  };
  // Любая правка делает прежнюю проверку недействительной.
  const edit = <T,>(set: (v: T) => void) => (v: T) => {
    setChecks(null);
    set(v);
  };
  const passed = checks?.some((c) => c.ok) ?? false;
  return (
    <form onSubmit={submit} className="space-y-4">
      <Field label={t("setup.dns")}>
        <Input autoFocus required placeholder="corp.example.local" value={dns} onChange={(e) => edit(setDns)(e.target.value)} />
      </Field>
      <Field label={t("setup.controllers")} hint={t("setup.controllersHint")}>
        <textarea required rows={2} className={textarea} placeholder="dc1.corp.example.local" value={controllers} onChange={(e) => edit(setControllers)(e.target.value)} />
      </Field>
      <Field label={t("setup.bind")} hint={t("setup.bindHint")}>
        <Input required autoComplete="off" placeholder="svc-pult@corp.example.local" value={bindDn} onChange={(e) => edit(setBindDn)(e.target.value)} />
      </Field>
      <Field label={t("setup.bindPassword")}>
        <Input required type="password" autoComplete="new-password" value={password} onChange={(e) => edit(setPassword)(e.target.value)} />
      </Field>
      <Field label={t("setup.tls")}>
        <Select value={tls} onChange={(e) => edit(setTls)(e.target.value)}>
          <option value="ldaps">LDAPS (636)</option>
          <option value="starttls">StartTLS (389)</option>
        </Select>
      </Field>
      <Field label={t("setup.ca")} hint={t("setup.caHint")}>
        <textarea rows={3} className={textarea} placeholder="-----BEGIN CERTIFICATE-----" value={ca} onChange={(e) => edit(setCa)(e.target.value)} />
      </Field>
      {test.error && <ErrorBox error={test.error} />}
      {checks && (
        <ul className="space-y-2 text-sm">
          {checks.map((c) => (
            <li key={c.controller}>
              <Badge tone={c.ok ? "ok" : "bad"}>{c.ok ? t("setup.ok") : t("setup.failed")}</Badge> <span className="font-mono text-xs">{c.controller}</span>
              {c.hint ? <div className="mt-1 text-bad">{c.hint}</div> : null}
              {c.error ? <div className="mt-1 break-words font-mono text-xs text-muted">{c.error}</div> : null}
            </li>
          ))}
        </ul>
      )}
      <div className="flex gap-2">
        <Button type="submit" className="flex-1 py-2.5" disabled={test.isPending}>
          {test.isPending ? t("setup.testing") : t("setup.test")}
        </Button>
        <Button type="button" variant="primary" className="flex-1 py-2.5" disabled={!passed} onClick={() => onDone(domain())}>
          {t("setup.next")}
        </Button>
      </div>
    </form>
  );
}

function AdminStep({ code, domain }: { code: string; domain: SetupDomain | null }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [login, setLogin] = useState("");
  const [password, setPassword] = useState("");
  const finish = useMutation({
    mutationFn: () =>
      ok(api.POST("/api/setup/complete", { headers: { "X-Setup-Token": code }, body: { domain, admin_login: login, admin_password: password } })),
    onSuccess: async () => {
      setPassword("");
      await qc.invalidateQueries();
    },
    onError: () => setPassword(""),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (login.trim() && password) finish.mutate();
  };
  return (
    <form onSubmit={submit} className="space-y-4">
      <p className="text-sm text-muted">{t("setup.adminText")}</p>
      <Field label={t("login.account")} hint={t("login.accountHint")}>
        <Input autoFocus autoComplete="username" value={login} onChange={(e) => setLogin(e.target.value)} />
      </Field>
      <Field label={t("login.password")}>
        <Input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
      </Field>
      {finish.error && <ErrorBox error={finish.error} />}
      <Button type="submit" variant="primary" className="w-full py-2.5" disabled={finish.isPending || !login.trim() || !password}>
        {finish.isPending ? t("setup.finishing") : t("setup.finish")}
      </Button>
    </form>
  );
}

export function SetupWizard({ status }: { status: SetupStatus }) {
  const { t } = useTranslation();
  const [code, setCode] = useState<string | null>(null);
  const [domain, setDomain] = useState<SetupDomain | null>(null);
  const step = !code ? "code" : !status.has_domain && !domain ? "domain" : "admin";
  return (
    <div className="flex min-h-screen items-start justify-center px-4 py-[8vh]">
      <div className="w-full max-w-md rounded-2xl border border-line bg-paper p-8 shadow-sm">
        <div className="flex items-start justify-between">
          <Logo />
          <LanguagePicker />
        </div>
        <h1 className="mt-5 text-xl font-semibold">{t("setup.title")}</h1>
        <p className="mb-6 text-sm text-muted">{t(`setup.steps.${step}`)}</p>
        {step === "code" && <CodeStep onDone={setCode} />}
        {step === "domain" && code && <DomainStep code={code} onDone={setDomain} />}
        {step === "admin" && code && <AdminStep code={code} domain={domain} />}
      </div>
    </div>
  );
}
