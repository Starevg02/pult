"""Чистая установка пакета -> мастер -> организация -> первая заявка, с замером (приёмка этапа 6).

Запускается из deb-wizard-e2e.sh.

argv: setup_code dc_endpoint ca_pem_file t0_epoch; passwords: E2E_SVC_PASS, E2E_USER_PASS (environment).
"""
import http.cookiejar
import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.request

code, dc, ca_file, t0 = sys.argv[1:5]
svc_pass, user_pass = os.environ["E2E_SVC_PASS"], os.environ["E2E_USER_PASS"]
t0 = float(t0)
BASE = "https://127.0.0.1"
ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE  # temporary self-signed certificate of a fresh install
jar = http.cookiejar.CookieJar()
op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar), urllib.request.HTTPSHandler(context=ctx))
failed = 0


def call(method, path, body=None, setup=False):
    req = urllib.request.Request(BASE + path, data=json.dumps(body).encode() if body is not None else None, method=method)
    req.add_header("Content-Type", "application/json")
    if setup:
        req.add_header("X-Setup-Token", code)
    csrf = next((c.value for c in jar if c.name.endswith("pult_csrf")), "")
    req.add_header("X-CSRF-Token", csrf)
    try:
        with op.open(req) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, raw[:300]


def check(label, ok, detail=""):
    global failed
    print(("ok  " if ok else "FAIL") + f" [{time.time() - t0:6.1f} s] {label}" + ("" if ok else f": {detail}"))
    if not ok:
        failed += 1
        print("=== E2E FAIL")
        sys.exit(1)


ca = open(ca_file, encoding="utf-8").read()
domain = {"dns_name": "pult.test", "controllers": [dc], "tls_mode": "ldaps", "bind_dn": "svc-pult@pult.test",
          "bind_password": svc_pass, "ca_cert_pem": ca}

s, r = call("GET", "/api/setup/status")
check("wizard open on a fresh install", s == 200 and r.get("needed") is True, r)
s, _ = call("POST", "/api/setup/verify", setup=True)
check("setup code from the journal accepted", s == 204, s)
s, r = call("POST", "/api/setup/test", domain, setup=True)
check("domain controllers answer", s == 200 and bool(r) and all(d.get("ok") is True for d in r), r)
s, r = call("POST", "/api/setup/complete", {"domain": domain, "admin_login": "admin", "admin_password": user_pass}, setup=True)
check("administrator signed in, setup complete", s == 204, r)
s, me = call("GET", "/api/me")
check("superadministrator", s == 200 and me.get("is_superadmin") is True, s)

# Первая синхронизация: сотрудники домена появляются без ручного запуска.
for _ in range(30):
    s, people = call("GET", "/api/phonebook/persons?q=%D0%98%D0%B2%D0%B0%D0%BD%D0%BE%D0%B2")
    if s == 200 and any(p.get("last_name") == "Иванов" for p in (people if isinstance(people, list) else people.get("items", []))):
        break
    time.sleep(1)
check("domain users synchronized", s == 200 and any(p.get("last_name") == "Иванов" for p in (people if isinstance(people, list) else people.get("items", []))), str(people)[:200])

s, org = call("GET", "/api/org")
check("organization wizard pending", s == 200 and org["setup_done"] is False, org)
s, r = call("PUT", "/api/admin/org/language", {"locale": "en"})
check("install language en, starter data translated", s == 200 and r["renamed"] > 20, r)
s, org = call("PUT", "/api/admin/org", {"name": "Demo Ltd", "modules": org["modules"], "setup_done": False})
check("organization name", s == 200, org)
s, site = call("POST", "/api/admin/sites", {"name": "Head office", "timezone": "Europe/Moscow", "is_active": True})
check("site", s == 201, site)
s, cats = call("GET", "/api/helpdesk/categories")
check("categories in English", s == 200 and any(c["name"] == "Computer and peripherals" for c in cats), cats)
s, org = call("PUT", "/api/admin/org", {"name": org["name"], "modules": org["modules"], "setup_done": True})
check("organization setup done", s == 200 and org["setup_done"] is True, org)
cat = next(c for c in cats if c["name"] == "Computer and peripherals")
s, t = call("POST", "/api/helpdesk/tickets", {"title": "Printer does not print", "description": "e2e", "category_id": cat["id"], "site_id": site["id"]})
check(f"first ticket #{t.get('number') if isinstance(t, dict) else '?'}", s == 201, t)
total = time.time() - t0
check(f"install to first ticket: {total:.0f} s (limit 900 s)", total < 900)
print("=== E2E OK")
