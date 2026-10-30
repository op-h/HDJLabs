<#
.SYNOPSIS
  Lets student PCs open exams downloaded from the teacher's PC without a SmartScreen warning.

.DESCRIPTION
  Windows SmartScreen only checks files downloaded from the Internet zone. This puts the
  teacher's address (or the classroom subnet) in the Local intranet zone, so exams downloaded
  from the HDJLabs download page are treated like files from a school file share.

  Run it once on each student PC (no admin needed; it only changes the current user), or have
  the lab admin set the same address with Group Policy:
    Computer/User Configuration > Administrative Templates > Windows Components >
    Internet Explorer > Internet Control Panel > Security Page > Site to Zone Assignment List
    Value name: http://192.168.1.20   Value: 1

.PARAMETER Teacher
  The teacher PC's address shown on the Live classroom page, e.g. 192.168.1.20, or a subnet
  pattern such as 192.168.1.* when the teacher's address changes.

.PARAMETER Remove
  Undo the change.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File lab-trust-teacher.ps1 -Teacher 192.168.1.20
#>
param(
    [Parameter(Mandatory = $true)][string]$Teacher,
    [switch]$Remove
)

$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings\ZoneMap\Ranges\HDJLabs"
if ($Remove) {
    Remove-Item $key -Force -ErrorAction SilentlyContinue
    Write-Host "Removed. Downloads from $Teacher are treated as Internet files again."
    return
}
if ($Teacher -notmatch '^\d{1,3}(\.(\d{1,3}|\*)){3}$') {
    throw "Use an IPv4 address such as 192.168.1.20 or a pattern such as 192.168.1.*"
}
New-Item $key -Force | Out-Null
New-ItemProperty $key -Name ":Range" -Value $Teacher -Force | Out-Null
New-ItemProperty $key -Name "http" -Value 1 -PropertyType DWord -Force | Out-Null

# Ask Windows itself (a new process reads the fresh setting).
$zone = powershell -NoProfile -Command "[System.Security.Policy.Zone]::CreateFromUrl('http://$($Teacher.Replace('*','1')):47880/download').SecurityZone"
Write-Host "Downloads from http://$Teacher are now in the '$zone' zone."
if ($zone -ne "Intranet") { Write-Warning "Windows did not accept the setting; a policy on this PC may override it." }
