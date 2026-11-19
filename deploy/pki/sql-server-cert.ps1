# Сертификат TLS для SQL Server от своего CA (docs/ad-setup.md, раздел «Сертификаты»).
# Запускать под администратором с ПК, где лежит CA; на SQL Server — через удалённый PowerShell.
#
#   $p = @{ Server = "sql01"; Fqdn = "sql01.corp.example.local"; Ip = "10.0.0.5"; Admin = "CORP\admin"; Dir = "C:\pki\requests" }
#   1) Запрос: ключ RSA создаётся на сервере (неэкспортируемый), сюда приходит только CSR
#      .\deploy\pki\sql-server-cert.ps1 -Stage Request @p
#   2) Подписать CSR своим CA (Git Bash):
#      deploy/pki/ca.sh sign /путь/к/ca /c/pki/requests/sql01-sql.csr
#   3) Установка: принять сертификат, дать службе SQL право на ключ, прописать в SQL Server
#      .\deploy\pki\sql-server-cert.ps1 -Stage Install @p              # без перезапуска
#      .\deploy\pki\sql-server-cert.ps1 -Stage Install @p -RestartSql  # с перезапуском SQL Server (клиенты — пауза на секунды)
#   Откат: .\deploy\pki\sql-server-cert.ps1 -Stage Rollback @p -RestartSql
param(
    [Parameter(Mandatory)][ValidateSet("Request", "Install", "Rollback")][string]$Stage,
    # Имя сервера SQL для удалённого PowerShell, его полное DNS-имя и IP (оба попадут в сертификат).
    [Parameter(Mandatory)][string]$Server,
    [Parameter(Mandatory)][string]$Fqdn,
    [Parameter(Mandatory)][string]$Ip,
    # Учётка администратора (DOMAIN\user); пароль спросит окно Windows.
    [Parameter(Mandatory)][string]$Admin,
    # Каталог запросов и сертификатов (тот же, где ca.sh sign оставит .crt) и журнала.
    [Parameter(Mandatory)][string]$Dir,
    # Имя экземпляра SQL Server (MSSQLSERVER, SQLEXPRESS…). Пусто — единственный экземпляр на сервере.
    [string]$Instance = "",
    [switch]$RestartSql
)
$ErrorActionPreference = "Stop"
$csrPath = Join-Path $Dir "$Server-sql.csr"
$crtPath = Join-Path $Dir "$Server-sql.crt"
New-Item -ItemType Directory -Force $Dir | Out-Null
# Журнал — чтобы ошибка не пропала, если окно закроется (пароли в него не попадают: Get-Credential не пишет).
Start-Transcript -Path (Join-Path $Dir "sql-server-cert.log") -Append | Out-Null
trap {
    Write-Host "`nОШИБКА: $_" -ForegroundColor Red
    Write-Host "Журнал: $(Join-Path $Dir 'sql-server-cert.log')"
    Stop-Transcript | Out-Null
    Read-Host "Нажмите Enter, чтобы закрыть"
    exit 1
}
$cred = Get-Credential -UserName $Admin -Message "Администратор домена для $Server"
if (-not $cred) { throw "учётные данные не введены" }

# Экземпляр SQL Server: служба, учётка службы и ветка реестра с настройками TLS.
# Экземпляр по умолчанию — служба MSSQLSERVER, именованный — MSSQL$<имя>.
$findSql = {
    param($Instance)
    $names = (Get-Item "HKLM:\SOFTWARE\Microsoft\Microsoft SQL Server\Instance Names\SQL").Property
    if (-not $names) { throw "на сервере нет экземпляров SQL Server" }
    if (-not $Instance) {
        if (@($names).Count -ne 1) { throw "экземпляров несколько ($($names -join ', ')) — укажите -Instance" }
        $Instance = @($names)[0]
    }
    if ($names -notcontains $Instance) { throw "экземпляра $Instance нет; есть: $($names -join ', ')" }
    $id = (Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Microsoft SQL Server\Instance Names\SQL").$Instance
    $svcName = if ($Instance -eq "MSSQLSERVER") { "MSSQLSERVER" } else { "MSSQL`$$Instance" }
    $svc = Get-CimInstance Win32_Service | Where-Object Name -eq $svcName
    if (-not $svc) { throw "служба $svcName не найдена" }
    [pscustomobject]@{
        Instance = $Instance
        Service  = $svcName
        Id       = $id
        Key      = "HKLM:\SOFTWARE\Microsoft\Microsoft SQL Server\$id\MSSQLServer\SuperSocketNetLib"
        Account  = $svc.StartName
        Version  = (Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Microsoft SQL Server\$id\Setup").Version
    }
}

switch ($Stage) {
    "Request" {
        $csr = Invoke-Command -ComputerName $Server -Credential $cred -ArgumentList $Fqdn, $Ip -ScriptBlock {
            param($Fqdn, $Ip)
            $ErrorActionPreference = "Stop"
            # KeySpec=1 и провайдер SChannel — так SQL Server принимает ключ во всех версиях.
            $inf = @'
[Version]
Signature="$Windows NT$"
[NewRequest]
Subject = "CN=__FQDN__"
KeyAlgorithm = RSA
KeyLength = 3072
HashAlgorithm = SHA256
KeySpec = 1
KeyUsage = 0xa0
MachineKeySet = TRUE
Exportable = FALSE
ProviderName = "Microsoft RSA SChannel Cryptographic Provider"
ProviderType = 12
RequestType = PKCS10
FriendlyName = "SQL Server TLS (Pult internal CA)"
[EnhancedKeyUsageExtension]
OID = 1.3.6.1.5.5.7.3.1
[Extensions]
2.5.29.17 = "{text}"
_continue_ = "dns=__FQDN__&"
_continue_ = "ipaddress=__IP__&"
'@ -replace "__FQDN__", $Fqdn -replace "__IP__", $Ip
            $tmp = Join-Path $env:TEMP "pult-sql-$([guid]::NewGuid())"
            New-Item -ItemType Directory $tmp | Out-Null
            Set-Content -Path "$tmp\req.inf" -Value $inf -Encoding ASCII
            $out = certreq -new -q -f "$tmp\req.inf" "$tmp\req.csr" 2>&1
            if ($LASTEXITCODE -ne 0) { throw "certreq -new: $out" }
            $text = Get-Content "$tmp\req.csr" -Raw
            Remove-Item -Recurse -Force $tmp
            $text
        }
        New-Item -ItemType Directory -Force $Dir | Out-Null
        Set-Content -Path $csrPath -Value $csr -Encoding ASCII
        Write-Host "Запрос: $csrPath (ключ остался на $Server, в хранилище компьютера, неэкспортируемый)."
        Write-Host "Дальше — подписать его: deploy/pki/ca.sh sign ... $Server-sql.csr"
    }
    "Install" {
        if (-not (Test-Path $crtPath)) { throw "нет $crtPath — сначала подпишите запрос (ca.sh sign)" }
        $crt = Get-Content $crtPath -Raw
        $info = Invoke-Command -ComputerName $Server -Credential $cred -ArgumentList $crt, $findSql.ToString(), $RestartSql.IsPresent, $Instance -ScriptBlock {
            param($Crt, $FindSql, $Restart, $Instance)
            $ErrorActionPreference = "Stop"
            $sql = & ([scriptblock]::Create($FindSql)) $Instance
            certutil -pulse | Out-Null   # подтянуть корень CA из AD, если ещё не пришёл
            $tmp = Join-Path $env:TEMP "pult-sql-$([guid]::NewGuid()).crt"
            Set-Content -Path $tmp -Value $Crt -Encoding ASCII
            $out = certreq -accept -q -machine $tmp 2>&1
            if ($LASTEXITCODE -ne 0) { throw "certreq -accept: $out" }
            $c = New-Object Security.Cryptography.X509Certificates.X509Certificate2 $tmp
            Remove-Item $tmp
            $cert = Get-Item "Cert:\LocalMachine\My\$($c.Thumbprint)"
            if (-not $cert.HasPrivateKey) { throw "сертификат принят без закрытого ключа — запрос делался не на этом сервере?" }
            # Право чтения закрытого ключа — только учётке службы SQL Server.
            $container = $cert.PrivateKey.CspKeyContainerInfo.UniqueKeyContainerName
            $keyFile = Join-Path "$env:ProgramData\Microsoft\Crypto\RSA\MachineKeys" $container
            icacls $keyFile /grant "$($sql.Account):R" | Out-Null
            # Прежнее значение — для отката.
            $prev = (Get-ItemProperty $sql.Key).Certificate
            Set-ItemProperty $sql.Key -Name "Certificate.PultPrevious" -Value "$prev"
            Set-ItemProperty $sql.Key -Name "Certificate" -Value $cert.Thumbprint.ToLower()
            if ($Restart) { Restart-Service -Name $sql.Service -Force }
            [pscustomobject]@{
                Thumbprint = $cert.Thumbprint; NotAfter = $cert.NotAfter; Sql = $sql.Version; Account = $sql.Account
                Previous = $prev; Restarted = [bool]$Restart; State = (Get-Service -Name $sql.Service).Status; Instance = $sql.Instance
            }
        }
        $info | Format-List Instance, Thumbprint, NotAfter, Sql, Account, Previous, Restarted, State
        if (-not $RestartSql) { Write-Host "SQL Server не перезапущен: новый сертификат заработает после перезапуска (-Stage Install -RestartSql или вручную)." }
    }
    "Rollback" {
        Invoke-Command -ComputerName $Server -Credential $cred -ArgumentList $findSql.ToString(), $RestartSql.IsPresent, $Instance -ScriptBlock {
            param($FindSql, $Restart, $Instance)
            $sql = & ([scriptblock]::Create($FindSql)) $Instance
            $prev = (Get-ItemProperty $sql.Key)."Certificate.PultPrevious"
            Set-ItemProperty $sql.Key -Name "Certificate" -Value "$prev"
            if ($Restart) { Restart-Service -Name $sql.Service -Force }
            "Сертификат SQL Server возвращён: '$prev' (пусто — самоподписанный). Перезапуск: $Restart"
        }
    }
}

Stop-Transcript | Out-Null
