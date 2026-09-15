param([Parameter(Mandatory=$true)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
$source = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Windows.Forms;
using System.Drawing;
class BackgroundUiProbe {
    [STAThread] static void Main(string[] args) {
        var form = new Form { Width=960, Height=720, ShowInTaskbar=false,
            StartPosition=FormStartPosition.Manual, Location=new Point(-4000,-4000), Opacity=0 };
        var document = new RichTextBox { Dock=DockStyle.Fill, Font=new Font("Microsoft YaHei UI", 11) };
        document.Text = File.ReadAllText(args[0]); form.Controls.Add(document);
        form.Shown += (s,e) => {
            var reader = new Thread(() => {
                Console.WriteLine("ready"); Console.Out.Flush();
                while (Console.ReadLine() == "probe") {
                    var values = new double[250];
                    for (int i=0; i<values.Length; i++) {
                        int sequence=i; var done = new ManualResetEvent(false);
                        var clock=Stopwatch.StartNew();
                        document.BeginInvoke((Action)(() => {
                            document.SelectionStart=(sequence*137)%Math.Max(1,document.TextLength);
                            document.SelectedText="CarbonPaper test input ";
                            document.ScrollToCaret(); document.Update(); done.Set();
                        }));
                        if (!done.WaitOne(5000)) Environment.Exit(2);
                        values[i]=clock.Elapsed.TotalMilliseconds; done.Dispose(); Thread.Sleep(40);
                    }
                    Array.Sort(values);
                    Console.WriteLine("{\"samples\":250,\"p95_ms\":" + values[237].ToString(System.Globalization.CultureInfo.InvariantCulture)
                        + ",\"max_ms\":" + values[249].ToString(System.Globalization.CultureInfo.InvariantCulture) + "}");
                    Console.Out.Flush();
                }
                form.BeginInvoke((Action)(() => form.Close()));
            });
            reader.IsBackground=true; reader.Start();
        };
        Application.Run(form);
    }
}
'@
$probe = Join-Path $OutputDirectory 'background-ui-probe.exe'
Add-Type -TypeDefinition $source -ReferencedAssemblies System.Windows.Forms,System.Drawing -OutputAssembly $probe -OutputType ConsoleApplication
Write-Output $probe
