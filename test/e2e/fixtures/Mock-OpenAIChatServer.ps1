param(
    [Parameter(Mandatory)][int]$Port,
    [Parameter(Mandatory)][string]$LogPath
)

$ErrorActionPreference = 'Stop'
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
Add-Content -LiteralPath $LogPath -Value "READY|$Port" -Encoding utf8

try {
    while ($true) {
        $client = $listener.AcceptTcpClient()
        try {
            $stream = $client.GetStream()
            $buffer = [byte[]]::new(8192)
            $requestBytes = [System.IO.MemoryStream]::new()
            $headerEnd = -1
            $contentLength = 0

            while ($true) {
                $read = $stream.Read($buffer, 0, $buffer.Length)
                if ($read -le 0) {
                    break
                }
                $requestBytes.Write($buffer, 0, $read)
                $all = $requestBytes.ToArray()
                if ($headerEnd -lt 0) {
                    $requestText = [System.Text.Encoding]::ASCII.GetString($all)
                    $headerEnd = $requestText.IndexOf("`r`n`r`n", [System.StringComparison]::Ordinal)
                    if ($headerEnd -ge 0) {
                        $headers = $requestText.Substring(0, $headerEnd)
                        if ($headers -match '(?im)^Content-Length:\s*(\d+)\s*$') {
                            $contentLength = [int]$Matches[1]
                        }
                    }
                }
                if ($headerEnd -ge 0 -and $all.Length -ge ($headerEnd + 4 + $contentLength)) {
                    break
                }
            }

            $all = $requestBytes.ToArray()
            $headerText = [System.Text.Encoding]::ASCII.GetString($all, 0, $headerEnd)
            $path = ($headerText.Split("`r`n")[0] -split ' ')[1]
            $body = if ($contentLength -gt 0) {
                [System.Text.Encoding]::UTF8.GetString($all, $headerEnd + 4, $contentLength)
            }
            else {
                ''
            }
            $request = @{ path = $path; body = $body } | ConvertTo-Json -Compress
            Add-Content -LiteralPath $LogPath -Value "REQUEST|$request" -Encoding utf8

            $first = @{
                id = 'chatcmpl-ite2e'
                object = 'chat.completion.chunk'
                created = 1
                model = 'ite2e-model'
                choices = @(@{
                    index = 0
                    delta = @{ role = 'assistant'; content = 'BYOM fixture response' }
                    finish_reason = $null
                })
            } | ConvertTo-Json -Depth 8 -Compress
            $last = @{
                id = 'chatcmpl-ite2e'
                object = 'chat.completion.chunk'
                created = 1
                model = 'ite2e-model'
                choices = @(@{
                    index = 0
                    delta = @{}
                    finish_reason = 'stop'
                })
            } | ConvertTo-Json -Depth 8 -Compress
            $responseBody = "data: $first`r`n`r`ndata: $last`r`n`r`ndata: [DONE]`r`n`r`n"
            $responseBytes = [System.Text.Encoding]::UTF8.GetBytes($responseBody)
            $headers = "HTTP/1.1 200 OK`r`nContent-Type: text/event-stream`r`nContent-Length: $($responseBytes.Length)`r`nConnection: close`r`n`r`n"
            $headerBytes = [System.Text.Encoding]::ASCII.GetBytes($headers)
            $stream.Write($headerBytes, 0, $headerBytes.Length)
            $stream.Write($responseBytes, 0, $responseBytes.Length)
            $stream.Flush()
        }
        catch {
            Add-Content -LiteralPath $LogPath -Value "ERROR|$($_.Exception.Message)" -Encoding utf8
        }
        finally {
            $client.Dispose()
        }
    }
}
finally {
    $listener.Stop()
}
