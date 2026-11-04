// YanChu insert spike —— 假目标窗口
// 一个真实的前台 Win32 窗口 + 一个 TextBox，每 150ms 把文本框内容落到 dump 文件。
// 这样测试脚本可以「真的往一个前台窗口里打字」，然后把结果读回来断言。
using System;
using System.Drawing;
using System.IO;
using System.Windows.Forms;

public class TargetForm : Form
{
    private readonly TextBox _box;
    private readonly string _dump;

    public TargetForm(string dump)
    {
        _dump = dump;
        Text = "YanChu Insert Target";
        Width = 760;
        Height = 320;
        StartPosition = FormStartPosition.CenterScreen;

        _box = new TextBox();
        _box.Multiline = true;
        _box.Dock = DockStyle.Fill;
        _box.Font = new Font("Microsoft YaHei UI", 16f);
        _box.BorderStyle = BorderStyle.None;
        Controls.Add(_box);

        var timer = new Timer();
        timer.Interval = 150;
        timer.Tick += delegate
        {
            try { File.WriteAllText(_dump, _box.Text); }
            catch { }
        };
        timer.Start();

        Shown += delegate
        {
            Activate();
            _box.Focus();
            _box.SelectionStart = _box.TextLength;
        };
    }
}

public static class Program
{
    [STAThread]
    public static void Main(string[] args)
    {
        Application.EnableVisualStyles();
        string dump = args.Length > 0 ? args[0] : "target-dump.txt";
        Application.Run(new TargetForm(dump));
    }
}
