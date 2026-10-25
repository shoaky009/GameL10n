using System;
using System.IO;
using System.Linq;
using System.Text;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Security.Cryptography;

// v31 string literals are separate from symbol names and serialized Unity objects.
// This reviewed selection is tied to the complete non-literal metadata tables.
// New builds require a new selection review; readable strings alone are not display evidence.
public sealed class Il2CppText
{
    const int HeaderSize = 256;
    const string TablesHash = "0a1ddbb1c993470ed5094755f8574fc4c1de9570c6678bd4a265dc5005631daa";
    static readonly UTF8Encoding Utf8 = new(false, true);
    static readonly Dictionary<int, string> Selected = CreateSelection();
    readonly byte[] raw;
    readonly int table, pool, poolSize;
    public IReadOnlyDictionary<int, string> Texts { get; }

    static Dictionary<int, string> CreateSelection()
    {
        var result = new Dictionary<int, string>();
        void Add(string context, params int[] indices) { foreach (int i in indices) result.Add(i, context); }
        Add("新手教学对白；含与按键/道具名拼接的句子片段，不补写缺失变量",
            1571, 1572, 1574, 6365, 6367, 8269, 8278, 8553, 12114, 12320, 12322, 12327);
        Add("精通界面/教程；部分句子与强调文字或数值拼接",
            312, 528, 587, 1155, 2709, 7341, 7453, 10981, 12360, 15745);
        Add("武器详情/属性标签；保留标记和数值占位符",
            82, 83, 141, 142, 144, 157, 158, 1357, 1994, 2105, 2989, 3991, 9784, 12484);
        Add("操作提示", 12337, 12339, 12340);
        // Review complete display consumers, including the continuations of a concatenated sentence.
        // Existing contexts above stay fixed so already translated units keep their memory keys.
        Add("新手教学对白；来自移动、背包、属性和技能教程的拼接片段，保留原换行，不补写按键、数值或其他片段",
            114, 115, 116, 118, 122, 124, 130, 136, 322, 340, 341, 347, 355, 380, 390, 398, 444,
            478, 479, 517, 545, 574, 575, 581, 586, 616, 647, 1424, 1570, 1573, 3041, 3259, 3309,
            3526, 4946, 5255, 5359, 5746, 5751, 6119, 6363, 7310, 7425, 7427, 7496, 7724, 8055,
            8528, 8532, 8533, 8534, 9577, 9613, 9732, 9778, 10263, 10583, 11066, 11090, 12115, 12138, 14470);
        Add("诅咒教程；标题或与图标拼接的说明片段，保留图标和固定数值标记",
            378, 449, 462, 1578, 3427, 4450, 7007);
        Add("技能说明；施法时间、持续时间、法力消耗和恢复的显示模板或计量单位，保留参数和标记",
            53, 54, 220, 225, 269, 304, 305, 1162, 1181, 5165, 6550, 9148, 9152, 9395, 9400,
            14516, 17432, 17436);
        Add("效果说明；环绕物、光环、伙伴、灌注等动态说明的拼接片段，不补写其他片段或数值",
            67, 72, 128, 273, 277, 389, 401, 570, 571, 572, 579, 580, 620, 1365, 3303,
            4413, 4414, 5067, 6019, 6021, 9053, 9054, 9118, 9121, 14192, 14193, 15000, 15002,
            17531, 17547, 17601, 17622, 17664);
        Add("角色面板；属性标题、统计名称或与数值拼接的标签，保留图标及排版标记",
            1625, 1626, 1629, 1630, 1631, 2706, 2708, 2730, 2894, 2918, 2919, 3117, 3217,
            3220, 3221, 3243, 3250, 3258, 3380, 3929, 3942, 3999, 4059, 4120, 4121, 4416, 4418,
            4429, 4451, 4454, 4456, 4530, 4561, 4844, 4850, 5110, 5130, 5166, 5170, 5171,
            5254, 5261, 5337, 5338, 5365, 5640, 5683, 5703, 5712, 5713, 5714, 5803, 5804,
            5805, 6011, 6161, 6163, 6164, 6204, 6466, 6574, 6834, 7158, 7212, 7252, 7325,
            7327, 7335, 7395, 7421, 7423, 7424, 7573, 7574, 7725, 7729, 7731, 7732, 8369,
            8530, 8556, 8656, 8657, 8661, 8829, 8853, 9119, 9120, 9150, 9179, 9339, 9340,
            9341, 9529, 9541, 9642, 9658, 9781, 9782, 9783, 9807, 9819, 9894, 9947, 9948,
            11056, 11082, 11084, 11132, 11216, 12099);
        Add("物品详情标签或界面操作提示；用作显示名称，不改输入绑定标识",
            3949, 4192, 4531, 4826, 6422, 6824, 7737, 8307, 8360, 8552, 11293, 11306, 11307, 12097);
        Add("角色对白；篝火 NPC 随机招呼和交谈，保留灵魂图标",
            2456, 4921, 4929, 6014, 6404, 7841, 7843, 10185, 10959, 12119, 12120, 12363);
        Add("状态效果详情；持续时间、剩余层数和生效说明的动态拼接片段，保留图标、换行及数值",
            66, 69, 143, 145, 146, 147, 150, 151, 152, 153, 154, 155, 156, 278, 279, 310, 442,
            548, 549, 557, 558, 560, 561, 563, 564, 578, 1165, 2866, 3273, 4529, 4532, 4533,
            4534, 4535, 4666, 5757, 6458, 8539, 8904, 9699, 11083, 11837, 14998, 16238, 16239,
            16327, 17048, 17663, 17666, 17667, 17668, 17742, 17743, 17744, 17745, 17746, 17747,
            17749, 17752, 17753, 17754, 17755);
        Add("装备详情；装备部位、套装、负重和调谐标签，保留参数和排版标记",
            56, 75, 92, 139, 644, 692, 1627, 2636, 3246, 4773, 5074, 5336, 5996, 6167, 8259, 9466);
        Add("提示框；元素伤害、武器升级及特殊效果的说明，保留参数和图标",
            568, 2732, 2733, 2734, 2735, 2736, 5108, 6173, 9777, 11057, 12107);
        Add("房间入口；地点名称及与稀有度等标签拼接的介绍片段，不补写其他片段",
            74, 2753, 2889, 2920, 3095, 3234, 4193, 5340, 6171, 7226, 7492, 9164, 9421, 9528,
            9568, 11250, 11854, 12125, 12134);
        // Deliberately exclude shared lookup/serialization keys: Skill (9576), Attack (2988),
        // Enemy (5169), Current (4443), Count (4373), Capacity (3890), Duration (5004), Radius (8870),
        // Cancel/Compare/Interact and panel row IDs such as SoulHearts/cursed_{0}.
        // Strength/Dexterity/Intelligence also enter a method-local dictionary in IncreaseStatsAutoPickup.Awake;
        // both its insertions and comparisons use these same literals, not serialized or external keys.
        // seconds (16327) is also an exception argument label in XsdDuration..ctor / SetTotalTimeoutCompletionTime;
        // those uses do not parse it. Singular second (16321) is a graph input key and remains excluded.
        // Keep room-routing names (Tavern/Pawn Shop), rarity keys and pure bracket expressions unchanged.
        return result;
    }

    public static bool IsPath(string path) => Path.GetFileName(path).Equals("global-metadata.dat", StringComparison.OrdinalIgnoreCase);
    public static string Id(string path, int index) => path + "::literal:" + index;
    static int I(byte[] bytes, int at) => BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(at, 4));
    static void W(byte[] bytes, int at, int value) => BinaryPrimitives.WriteInt32LittleEndian(bytes.AsSpan(at, 4), value);

    public Il2CppText(byte[] bytes)
    {
        raw = bytes;
        if (raw.Length < HeaderSize || I(raw, 0) != unchecked((int)0xFAB11BAF) || I(raw, 4) != 31)
            throw new InvalidDataException("未支持的 IL2CPP 元数据版本；此文字规则需要 v31。");
        table = I(raw, 8); pool = I(raw, 16); poolSize = I(raw, 20);
        if (table != HeaderSize || I(raw, 12) != 17876 * 8 || pool != table + I(raw, 12)
            || poolSize < 0 || pool + (long)poolSize > raw.Length)
            throw new InvalidDataException("IL2CPP 字面量表布局不匹配。");
        int end = pool + poolSize;
        int previousEnd = end;
        for (int h = 24; h < HeaderSize; h += 8)
        {
            int offset = I(raw, h), length = I(raw, h + 4);
            if (length < 0 || offset < previousEnd || offset + (long)length > raw.Length)
                throw new InvalidDataException("IL2CPP 元数据表边界或顺序无效。");
            previousEnd = offset + length;
        }
        if (Convert.ToHexStringLower(SHA256.HashData(raw.AsSpan(end))) != TablesHash)
            throw new InvalidDataException("IL2CPP 元数据与已审核的显示文字规则不匹配；不能套用其他版本的字面量索引。");
        var texts = new Dictionary<int, string>();
        for (int i = 0; i < I(raw, 12) / 8; i++)
        {
            int length = I(raw, table + i * 8), offset = I(raw, table + i * 8 + 4);
            if (length < 0 || offset < 0 || offset + (long)length > poolSize)
                throw new InvalidDataException("IL2CPP 字面量越界：" + i);
            if (Selected.ContainsKey(i)) texts.Add(i, Utf8.GetString(raw, pool + offset, length));
        }
        Texts = texts;
    }

    public IEnumerable<ResourceUnit> Units(string path) => Texts.Select(t => new ResourceUnit(
        Id(path, t.Key), path, "StringLiteral:" + t.Key, t.Value, "IL2CPP / " + Selected[t.Key], null,
        Selected[t.Key].StartsWith("新手", StringComparison.Ordinal) || Selected[t.Key].StartsWith("角色对白", StringComparison.Ordinal) ? "dialogue" : "description",
        new[] { "##", "\n•", "//", "((x0.50))" }.Where(t.Value.Contains).ToArray() is { Length: > 0 } tokens ? tokens : null));

    public IEnumerable<string> Changed(string path, IReadOnlyDictionary<string, string> translations) => Texts
        .Where(t => translations.TryGetValue(Id(path, t.Key), out var value) && value != t.Value)
        .Select(t => translations[Id(path, t.Key)]);

    public byte[] Rebuild(string path, IReadOnlyDictionary<string, string> translations)
    {
        using var added = new MemoryStream();
        var updates = new List<(int index, int offset, int length)>();
        foreach (var (index, original) in Texts)
        {
            if (!translations.TryGetValue(Id(path, index), out var text) || text == original) continue;
            if (string.IsNullOrWhiteSpace(text) || text.Contains('\0')) throw new InvalidDataException("字面量译文为空或含空字符。");
            var encoded = Utf8.GetBytes(text);
            updates.Add((index, checked(poolSize + (int)added.Length), encoded.Length));
            added.Write(encoded);
        }
        if (updates.Count == 0) return raw;
        // Preserve original/possibly overlapping strings. Only changed values are appended to the
        // same pool; literal indices stay fixed. Shift absolute header offsets, never native code.
        while ((poolSize + added.Length) % 4 != 0) added.WriteByte(0);
        int delta = checked((int)added.Length), oldEnd = pool + poolSize;
        var result = new byte[checked(raw.Length + delta)];
        raw.AsSpan(0, oldEnd).CopyTo(result);
        added.GetBuffer().AsSpan(0, delta).CopyTo(result.AsSpan(oldEnd));
        raw.AsSpan(oldEnd).CopyTo(result.AsSpan(oldEnd + delta));
        W(result, 20, checked(poolSize + delta));
        for (int h = 24; h < HeaderSize; h += 8) W(result, h, checked(I(raw, h) + delta));
        foreach (var change in updates)
        {
            W(result, table + change.index * 8, change.length);
            W(result, table + change.index * 8 + 4, change.offset);
        }
        _ = new Il2CppText(result);
        return result;
    }
}
