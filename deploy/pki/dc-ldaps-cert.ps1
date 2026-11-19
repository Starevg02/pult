# Сертификат LDAPS для контроллера домена от своего CA (docs/ad-setup.md, раздел «Сертификаты»).
# Нужен, если в домене нет AD CS: без сертификата контроллер сбрасывает соединения на порт 636.
# Запускать под администратором домена с ПК, где лежит CA; на контроллер — через удалённый PowerShell.
#
#   $p = @{ Server = "dc2"; Fqdn = "dc2.corp.example.local"; Ip = "10.0.0.2"; Admin = "CORP\admin"; Dir = "C:\pki\requests" }
#   1) Запрос: ключ RSA создаётся на контроллере (неэкспортируемый), сюда приходит только CSR
#      .\deploy\pki\dc-ldaps-cert.ps1 -Stage Request @p
#   2) Подписать CSR своим CA (Git Bash):
#      deploy/pki/ca.sh sign /путь/к/ca /c/pki/requests/dc2-ldaps.csr
#   3) Установка: принять сертификат в хранилище компьютера и попросить AD подхватить его
#      без перезагрузки (renewServerCertificate). Перезапуска служб нет.
#      .\deploy\pki\dc-ldaps-cert.ps1 -Stage Install @p
#   Откат: .\deploy\pki\dc-ldaps-cert.ps1 -Stage Rollback @p   (удаляет установленный сертификат)
param(
    [Parameter(Mandatory)][ValidateSet("Request", "Install", "Rollback")][string]$Stage,
    # Имя контроллера для удалённого PowerShell, его полное DNS-имя и IP (оба попадут в сертификат).
    [Parameter(Mandatory)][string]$Server,
    [Parameter(Mandatory)][string]$Fqdn,
    [Parameter(Mandatory)][string]$Ip,
    # Учётка администратора домена (DOMAIN\user); пароль спросит окно Windows.
    [Parameter(Mandatory)][string]$Admin,
    # Каталог запросов и сертификатов (тот же, где ca.sh sign оставит .crt) и журнала.
    [Parameter(Mandatory)][string]$Dir
)
$ErrorActionPreference = "Stop"
$csrPath = Join-Path $Dir "$Server-ldaps.csr"
$crtPath = Join-Path $Dir "$Server-ldaps.crt"
$thumbPath = Join-Path $Dir "$Server-ldaps.thumbprint"
New-Item -ItemType Directory -Force $Dir | Out-Null
# Журнал — чтобы ошибка не пропала, если окно закроется (пароли в него не попадают: Get-Credential не пишет).
Start-Transcript -Path (Join-Path $Dir "dc-ldaps-cert.log") -Append | Out-Null
trap {
    Write-Host "`nОШИБКА: $_" -ForegroundColor Red
    Write-Host "Журнал: $(Join-Path $Dir 'dc-ldaps-cert.log')"
    Stop-Transcript | Out-Null
    Read-Host "Нажмите Enter, чтобы закрыть"
    exit 1
}
$cred = Get-Credential -UserName $Admin -Message "Администратор домена для $Server"
if (-not $cred) { throw "учётные данные не введены" }

# AD подхватывает новый сертификат LDAPS без перезагрузки по операции renewServerCertificate на RootDSE.
$renew = {
    $root = [ADSI]"LDAP://localhost/RootDSE"
    $root.Put("renewServerCertificate", 1)
    $root.SetInfo()
}

switch ($Stage) {
    "Request" {
        $csr = Invoke-Command -ComputerName $Server -Credential $cred -ArgumentList $Fqdn, $Ip -ScriptBlock {
            param($Fqdn, $Ip)
            $ErrorActionPreference = "Stop"
            $isDc = (Get-WmiObject Win32_ComputerSystem).DomainRole -ge 4
            if (-not $isDc) { throw "$env:COMPUTERNAME — не контроллер домена" }
            # Ключ RSA задаёт сам провайдер SChannel (тип 12); KeyAlgorithm вместе с KeySpec
            # certreq на части версий Windows Server отвергает (0x8007000d).
            Write-Host "$env:COMPUTERNAME: $((Get-WmiObject Win32_OperatingSystem).Caption)"
            $inf = @'
[Version]
Signature="$Windows NT$"
[NewRequest]
Subject = "CN=__FQDN__"
KeyLength = 3072
HashAlgorithm = SHA256
KeySpec = 1
KeyUsage = 0xa0
MachineKeySet = TRUE
Exportable = FALSE
ProviderName = "Microsoft RSA SChannel Cryptographic Provider"
ProviderType = 12
RequestType = PKCS10
FriendlyName = "LDAPS (Pult internal CA)"
[EnhancedKeyUsageExtension]
OID = 1.3.6.1.5.5.7.3.1
[Extensions]
2.5.29.17 = "{text}"
_continue_ = "dns=__FQDN__&"
_continue_ = "ipaddress=__IP__&"
'@ -replace "__FQDN__", $Fqdn -replace "__IP__", $Ip
            $tmp = Join-Path $env:TEMP "pult-ldaps-$([guid]::NewGuid())"
            New-Item -ItemType Directory $tmp | Out-Null
            Set-Content -Path "$tmp\req.inf" -Value $inf -Encoding ASCII
            $out = certreq -new -q -f "$tmp\req.inf" "$tmp\req.csr" 2>&1
            if ($LASTEXITCODE -ne 0) { throw "certreq -new: $out" }
            $text = [IO.File]::ReadAllText("$tmp\req.csr")
            Remove-Item -Recurse -Force $tmp
            $text
        }
        Set-Content -Path $csrPath -Value $csr -Encoding ASCII
        Write-Host "Запрос: $csrPath (ключ остался на $Server, в хранилище компьютера, неэкспортируемый)."
        Write-Host "Дальше — подписать его: deploy/pki/ca.sh sign ... $Server-ldaps.csr"
    }
    "Install" {
        if (-not (Test-Path $crtPath)) { throw "нет $crtPath — сначала подпишите запрос (ca.sh sign)" }
        $crt = [IO.File]::ReadAllText($crtPath)
        $info = Invoke-Command -ComputerName $Server -Credential $cred -ArgumentList $crt, $renew.ToString() -ScriptBlock {
            param($Crt, $Renew)
            $ErrorActionPreference = "Stop"
            certutil -pulse | Out-Null   # подтянуть корень CA из AD, если ещё не пришёл
            $tmp = Join-Path $env:TEMP "pult-ldaps-$([guid]::NewGuid()).crt"
            Set-Content -Path $tmp -Value $Crt -Encoding ASCII
            $out = certreq -accept -q -machine $tmp 2>&1
            if ($LASTEXITCODE -ne 0) { throw "certreq -accept: $out" }
            $c = New-Object Security.Cryptography.X509Certificates.X509Certificate2 $tmp
            Remove-Item $tmp
            $cert = Get-Item "Cert:\LocalMachine\My\$($c.Thumbprint)"
            if (-not $cert.HasPrivateKey) { throw "сертификат принят без закрытого ключа — запрос делался не на этом сервере?" }
            $chain = New-Object Security.Cryptography.X509Certificates.X509Chain
            $chain.ChainPolicy.RevocationMode = "Online"
            $trusted = $chain.Build($cert)
            & ([scriptblock]::Create($Renew))
            New-Object PSObject -Property @{
                Thumbprint = $cert.Thumbprint; NotAfter = $cert.NotAfter; Subject = $cert.Subject
                ChainOk = $trusted; ChainStatus = ($chain.ChainStatus | ForEach-Object { $_.StatusInformation.Trim() }) -join "; "
            }
        }
        Set-Content -Path $thumbPath -Value $info.Thumbprint -Encoding ASCII
        $info | Format-List Thumbprint, Subject, NotAfter, ChainOk, ChainStatus
        Write-Host "Сертификат установлен, AD попросили подхватить его (renewServerCertificate). Порт 636 — проверка с сервера Pult."
    }
    "Rollback" {
        if (-not (Test-Path $thumbPath)) { throw "нет $thumbPath — какой сертификат удалять, неизвестно" }
        $thumb = [IO.File]::ReadAllText($thumbPath).Trim()
        Invoke-Command -ComputerName $Server -Credential $cred -ArgumentList $thumb, $renew.ToString() -ScriptBlock {
            param($Thumb, $Renew)
            $out = certutil -delstore My $Thumb 2>&1
            if ($LASTEXITCODE -ne 0) { throw "certutil -delstore: $out" }
            & ([scriptblock]::Create($Renew))
            "Сертификат $Thumb удалён, AD попросили перечитать сертификаты."
        }
    }
}

Stop-Transcript | Out-Null
