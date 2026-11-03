using System.Buffers.Binary;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Text;

namespace GameL10n.Core.Development;

/// <summary>Reads declarations, never loads game assemblies or treats native layout as Unity serialization.</summary>
public static class UnitySchemaInspection
{
    // Returned only when the agent investigates Unity; these rules do not belong in every engine's prompt.
    internal const string Guidance = """
        Reuse asset readers/writers and the proven font path. Inventory dialogue/TextAsset, serialized UI and item/equipment/skill/effect text
        separately. Empty direct descriptions do not prove absence: follow referenced effects/status objects, inherited names, active/informative/
        duration/trigger descriptions and actual tooltip consumers. Method names are clues, not display evidence. Stripped serialization needs
        verified inheritance, bool alignment, PPtr widths and arrays with 0/1/multiple elements; unresolved type indices cannot imply 4-byte strides.
        For missing or partly translated runtime text, search exact visible phrases with mode=literals, then mode=references with matching metadata
        and GameAssembly. Inspect the whole consumer and its helpers via queryKind=method: sentence continuations, units, headings, tags, random
        dialogue variants and numeric templates. Follow actual code with inspect_pe_code. Check other consumers before replacing shared literals:
        UI words may also be input bindings, reflection properties, stat-context keys or serialization names. Exclude conflicting uses or isolate
        proven display uses; do not rename enum/input/lookup symbols or translate the whole table. Unknown references remain unverified.
        Keep literal indices and non-text tables intact, bind selections to the matching build, and declare engine-specific protectedTokens on units.
        Put display-format rules in the adapter, not generic translation validation. Record representative beginning/continuation/variant samples
        and concrete pendingCoverage. Roundtrip checks validate selected text only; they never prove complete runtime UI coverage.
        """;
    private sealed record Field(string Name, string? Type, int? TypeIndex, int DeclarationIndex,
        bool? IsStatic = null, bool? NotSerialized = null);
    private sealed record TextMethod(string Name, int Token);
    private sealed record Definition(int Index, string Name, string Kind, string? BaseType, int? BaseTypeIndex,
        IReadOnlyList<Field> Fields, int MethodCount, IReadOnlyList<TextMethod> TextMethods, int? MetadataOffset = null);

    public static async Task<object> InspectAsync(string path, string query, int offset, int limit, CancellationToken ct = default,
        string mode = "types")
    {
        if (query.Length > 256 || offset < 0 || limit is < 1 or > 8) throw new ArgumentException("类型查询范围无效。");
        if (mode is not ("types" or "literals")) throw new ArgumentException("未知的 Unity 查询模式。");
        await using var stream = File.OpenRead(path);
        if (stream.Length is < 8 or > 128 * 1024 * 1024) throw new InvalidDataException("类型文件必须在 8 字节到 128 MiB 之间。");
        var magic = new byte[8];
        await stream.ReadExactlyAsync(magic, ct);
        stream.Position = 0;
        List<Definition> types;
        string format;
        int? version = null;
        if (BinaryPrimitives.ReadUInt32LittleEndian(magic) == 0xFAB11BAF)
        {
            version = BinaryPrimitives.ReadInt32LittleEndian(magic.AsSpan(4));
            // These table offsets and row sizes are specific to v31. Other layouts must be
            // handled by a version-aware upstream parser, not silently interpreted as v31.
            if (version != 31) return new { status = "unsupported-version", format = "il2cpp", version,
                nextAction = "内置元数据查询目前支持 v31。使用对应版本的 IL2CPP 工具或匹配的托管/dummy DLL；不要套用 v31 表偏移。" };
            var bytes = new byte[(int)stream.Length];
            await stream.ReadExactlyAsync(bytes, ct);
            if (mode == "literals") return InspectLiterals(bytes, query, offset, limit, ct);
            types = ReadIl2Cpp(bytes, ct);
            format = "il2cpp";
        }
        else
        {
            if (mode == "literals") throw new InvalidDataException("字面量查询需要 IL2CPP global-metadata.dat；托管代码请检查方法 IL 的 ldstr。");
            using var pe = new PEReader(stream, PEStreamOptions.LeaveOpen);
            if (!pe.HasMetadata) throw new InvalidDataException("此文件不含托管元数据。原生 GameAssembly.dll 需要 IL2CPP 元数据及运行时注册表解析，不能当作托管 DLL。");
            types = ReadManaged(pe.GetMetadataReader(), ct);
            format = "managed";
        }

        var terms = query.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var selected = terms.Length == 0 ? types : terms.SelectMany(term =>
        {
            var exact = types.Where(type => type.Name.Equals(term, StringComparison.OrdinalIgnoreCase)
                || type.Name[(type.Name.LastIndexOf('.') + 1)..].Equals(term, StringComparison.OrdinalIgnoreCase)).ToArray();
            return exact.Length > 0 ? exact : types.Where(type => type.Name.Contains(term, StringComparison.OrdinalIgnoreCase));
        }).DistinctBy(type => type.Index).OrderBy(type => type.Index).ToList();
        var page = selected.Skip(offset).Take(limit).ToArray();
        var detail = terms.Length > 0;
        var byName = types.GroupBy(type => type.Name).ToDictionary(group => group.Key, group => group.ToArray(), StringComparer.Ordinal);
        var related = new List<Definition>();
        var visited = page.Select(type => type.Index).ToHashSet();
        if (detail)
            foreach (var type in page) AddRelated(type);
        // Include the base's own base/struct dependencies, bounded independently of file size.
        for (var i = 0; i < related.Count && i < 6; i++) AddRelated(related[i]);
        return new
        {
            status = "inspected", format, version, guidance = Guidance, totalTypes = types.Count, totalMatches = selected.Count,
            offset, nextOffset = offset + page.Length, more = offset + page.Length < selected.Count,
            types = page.Select(type => View(type, detail ? 64 : 0)).ToArray(),
            relatedTypes = related.Select(type => View(type, 24)).ToArray(),
            interpretation = format == "il2cpp"
                ? "字段按声明顺序排列；typeIndex 是运行时类型表索引，不是类型定义行号。已解析名称只来自 byvalTypeIndex 的精确匹配。数组/泛型索引可能未解析，需对应 GameAssembly 注册表或匹配 dummy DLL。元数据本身不提供序列化偏移或字段 static/NonSerialized 属性。"
                : "字段包含声明类型和 static/NotSerialized 标志。外部程序集类型未自动解析。dummy DLL 必须与游戏版本匹配；托管字段声明或 FieldOffset 均不能直接当作 Unity 资源序列化偏移。",
            nextAction = detail
                ? "结合资源 type tree 或真实对象验证继承、字段条件、长度与对齐。结构数组应核对元素成员，并选长度 0/1/多项的样本验证步长；未知类型不可默认按 4 字节跳过。textMethods 仅是按名称筛选的声明线索，不是调用关系或实际显示证据；描述字段为空但存在效果引用或 Description/Tooltip 方法时，追踪其关联对象及生成文本。"
                : "用 query 指定相关类型（可逗号分隔）查看字段及关联结构。分别检查对话、静态 UI、物品/技能说明和已知遗漏，避免全文扫描报告。"
        };

        void AddRelated(Definition type)
        {
            IEnumerable<string?> names = new[] { type.BaseType }.Concat(type.Fields.Select(field => field.Type));
            foreach (var name in names)
            {
                if (related.Count >= 6) return;
                if (name is null || !byName.TryGetValue(name.TrimEnd('[', ']', '&', '*'), out var candidates)) continue;
                foreach (var candidate in candidates)
                {
                    if (related.Count >= 6) return;
                    if (candidate.Name.StartsWith("System.", StringComparison.Ordinal)) continue;
                    if (name != type.BaseType && candidate.Kind is not ("struct" or "enum")) continue;
                    if (visited.Add(candidate.Index)) related.Add(candidate);
                }
            }
        }
    }

    private static object InspectLiterals(byte[] data, string query, int offset, int limit, CancellationToken ct)
    {
        if (data.Length < 24) throw new InvalidDataException("IL2CPP 字面量表头被截断。");
        int I(int at) => BinaryPrimitives.ReadInt32LittleEndian(data.AsSpan(at, 4));
        int table = I(8), tableBytes = I(12), pool = I(16), poolBytes = I(20);
        if (table < 24 || tableBytes < 0 || tableBytes % 8 != 0 || pool < table + (long)tableBytes
            || poolBytes < 0 || pool + (long)poolBytes > data.Length)
            throw new InvalidDataException("IL2CPP 字面量表范围无效。");
        var utf8 = new UTF8Encoding(false, true);
        var matches = new List<object>();
        int matched = 0;
        for (int index = 0; index < tableBytes / 8; index++)
        {
            ct.ThrowIfCancellationRequested();
            int length = I(table + index * 8), at = I(table + index * 8 + 4);
            if (length < 0 || at < 0 || at + (long)length > poolBytes)
                throw new InvalidDataException("IL2CPP 字面量范围无效：" + index);
            var text = utf8.GetString(data, pool + at, length);
            if (!text.Contains(query, StringComparison.OrdinalIgnoreCase)) continue;
            if (matched >= offset && matches.Count < limit)
                matches.Add(new { index, byteOffset = pool + at, byteLength = length,
                    text = text.Length > 2048 ? text[..2048] : text, truncated = text.Length > 2048 });
            matched++;
        }
        return new { status = "ok", format = "il2cpp", version = 31, mode = "literals", total = tableBytes / 8,
            matched, offset, nextOffset = offset + matches.Count < matched ? (int?)(offset + matches.Count) : null,
            literals = matches, guidance = Guidance,
            notice = "字面量同时包含显示文案、查找键和诊断信息。匹配文字不能证明它仅用于显示；结合截图/消费者逐项审核，不要整表翻译。" };
    }

    private static object View(Definition type, int fieldLimit) => new
    {
        type.Index, type.Name, type.Kind, type.BaseType, type.BaseTypeIndex, type.MetadataOffset,
        fieldCount = type.Fields.Count, fields = type.Fields.Take(fieldLimit).ToArray(),
        fieldsTruncated = type.Fields.Count > fieldLimit,
        unresolvedFieldTypes = type.Fields.Count(field => field.Type is null),
        type.MethodCount, textMethodCount = type.TextMethods.Count,
        textMethods = type.TextMethods.Take(fieldLimit > 0 ? 16 : 0).ToArray(),
        textMethodsTruncated = type.TextMethods.Count > (fieldLimit > 0 ? 16 : 0)
    };

    private static bool IsTextMethod(string name) =>
        name.Contains("Description", StringComparison.OrdinalIgnoreCase)
        || name.Contains("Tooltip", StringComparison.OrdinalIgnoreCase)
        || name.Contains("DisplayName", StringComparison.OrdinalIgnoreCase)
        || name.Contains("GetText", StringComparison.OrdinalIgnoreCase)
        || name.Contains("BuildText", StringComparison.OrdinalIgnoreCase)
        || name.Contains("FormatText", StringComparison.OrdinalIgnoreCase)
        || name.Contains("Localiz", StringComparison.OrdinalIgnoreCase);

    private static List<Definition> ReadIl2Cpp(byte[] data, CancellationToken ct)
    {
        if (data.Length < 168) throw new InvalidDataException("IL2CPP 元数据头被截断。");
        int I(int at) => BinaryPrimitives.ReadInt32LittleEndian(data.AsSpan(at, 4));
        int U(int at) => BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(at, 2));
        (int Start, int Count) Table(int header, int stride)
        {
            var start = I(header);
            var bytes = I(header + 4);
            if (start < 168 || bytes < 0 || bytes % stride != 0 || start + (long)bytes > data.Length)
                throw new InvalidDataException($"IL2CPP v31 表边界无效（头偏移 {header}）。");
            return (start, bytes / stride);
        }
        var strings = Table(24, 1);
        var methods = Table(48, 36);
        var fields = Table(96, 12);
        var definitions = Table(160, 88);
        var utf8 = new UTF8Encoding(false, true);
        string S(int index)
        {
            if (index < 0 || index >= strings.Count) throw new InvalidDataException("IL2CPP 字符串索引越界。");
            var at = strings.Start + index;
            var end = Array.IndexOf(data, (byte)0, at, Math.Min(strings.Count - index, 4096));
            if (end < 0) throw new InvalidDataException("IL2CPP 类型/字段名称未终止或过长。");
            return utf8.GetString(data, at, end - at);
        }
        var names = new string[definitions.Count];
        var byValue = new Dictionary<int, string>();
        for (var i = 0; i < names.Length; i++)
        {
            ct.ThrowIfCancellationRequested();
            var row = definitions.Start + i * 88;
            var ns = S(I(row + 4));
            names[i] = (ns.Length > 0 ? ns + "." : "") + S(I(row));
            var typeIndex = I(row + 8);
            if (typeIndex < 0 || !byValue.TryAdd(typeIndex, names[i]))
                throw new InvalidDataException("IL2CPP v31 类型索引无效或重复，不能可靠解释此元数据布局。");
        }
        var result = new List<Definition>(names.Length);
        for (var i = 0; i < names.Length; i++)
        {
            ct.ThrowIfCancellationRequested();
            var row = definitions.Start + i * 88;
            var start = I(row + 32);
            var count = U(row + 68);
            if (count > 0 && (start < 0 || start + (long)count > fields.Count))
                throw new InvalidDataException("IL2CPP 字段范围越界。");
            var members = new List<Field>(count);
            for (var k = 0; k < count; k++)
            {
                var field = fields.Start + (start + k) * 12;
                var index = I(field + 4);
                members.Add(new(S(I(field)), byValue.GetValueOrDefault(index), index, k));
            }
            var methodStart = I(row + 36);
            var methodCount = U(row + 64);
            if (methodCount > 0 && (methodStart < 0 || methodStart + (long)methodCount > methods.Count))
                throw new InvalidDataException("IL2CPP 方法范围越界。");
            var textMethods = new List<TextMethod>();
            for (var k = 0; k < methodCount; k++)
            {
                var method = methods.Start + (methodStart + k) * 36;
                var name = S(I(method));
                if (IsTextMethod(name)) textMethods.Add(new(name, I(method + 24)));
            }
            var parent = I(row + 16);
            var bits = I(row + 80);
            result.Add(new(i, names[i], (bits & 2) != 0 ? "enum" : (bits & 1) != 0 ? "struct" : "class",
                byValue.GetValueOrDefault(parent), parent < 0 ? null : parent, members, methodCount, textMethods, row));
        }
        return result;
    }

    private static List<Definition> ReadManaged(MetadataReader reader, CancellationToken ct)
    {
        var provider = new SignatureNames();
        var result = new List<Definition>();
        foreach (var handle in reader.TypeDefinitions)
        {
            ct.ThrowIfCancellationRequested();
            var type = reader.GetTypeDefinition(handle);
            var parent = provider.Entity(reader, type.BaseType);
            var members = type.GetFields().Select((fieldHandle, i) =>
            {
                var field = reader.GetFieldDefinition(fieldHandle);
                return new Field(reader.GetString(field.Name), field.DecodeSignature(provider, null), null, i,
                    (field.Attributes & FieldAttributes.Static) != 0, ((int)field.Attributes & 0x80) != 0);
            }).ToArray();
            var methods = type.GetMethods();
            var textMethods = methods.Select(methodHandle =>
            {
                var method = reader.GetMethodDefinition(methodHandle);
                return new TextMethod(reader.GetString(method.Name), MetadataTokens.GetToken(methodHandle));
            }).Where(method => IsTextMethod(method.Name)).ToArray();
            result.Add(new(MetadataTokens.GetRowNumber(handle), provider.GetTypeFromDefinition(reader, handle, 0),
                parent == "System.Enum" ? "enum" : parent == "System.ValueType" ? "struct" : "class", parent, null, members,
                methods.Count, textMethods));
        }
        return result;
    }

    private sealed class SignatureNames : ISignatureTypeProvider<string, object?>
    {
        public string? Entity(MetadataReader reader, EntityHandle handle) => handle.IsNil ? null : handle.Kind switch
        {
            HandleKind.TypeDefinition => GetTypeFromDefinition(reader, (TypeDefinitionHandle)handle, 0),
            HandleKind.TypeReference => GetTypeFromReference(reader, (TypeReferenceHandle)handle, 0),
            HandleKind.TypeSpecification => GetTypeFromSpecification(reader, null, (TypeSpecificationHandle)handle, 0),
            _ => null
        };
        public string GetTypeFromDefinition(MetadataReader reader, TypeDefinitionHandle handle, byte rawTypeKind)
        {
            var type = reader.GetTypeDefinition(handle);
            var parent = type.GetDeclaringType();
            return parent.IsNil ? Name(reader, type.Namespace, type.Name)
                : GetTypeFromDefinition(reader, parent, 0) + "+" + reader.GetString(type.Name);
        }
        public string GetTypeFromReference(MetadataReader reader, TypeReferenceHandle handle, byte rawTypeKind)
        {
            var type = reader.GetTypeReference(handle);
            return type.ResolutionScope.Kind == HandleKind.TypeReference
                ? GetTypeFromReference(reader, (TypeReferenceHandle)type.ResolutionScope, 0) + "+" + reader.GetString(type.Name)
                : Name(reader, type.Namespace, type.Name);
        }
        private static string Name(MetadataReader reader, StringHandle ns, StringHandle name) =>
            (reader.GetString(ns) is { Length: > 0 } prefix ? prefix + "." : "") + reader.GetString(name);
        public string GetTypeFromSpecification(MetadataReader reader, object? context, TypeSpecificationHandle handle, byte kind) =>
            reader.GetTypeSpecification(handle).DecodeSignature(this, context);
        public string GetArrayType(string elementType, ArrayShape shape) => elementType + "[" + new string(',', shape.Rank - 1) + "]";
        public string GetSZArrayType(string elementType) => elementType + "[]";
        public string GetByReferenceType(string elementType) => elementType + "&";
        public string GetPointerType(string elementType) => elementType + "*";
        public string GetPinnedType(string elementType) => elementType;
        public string GetModifiedType(string modifier, string unmodifiedType, bool isRequired) => unmodifiedType;
        public string GetGenericInstantiation(string genericType, System.Collections.Immutable.ImmutableArray<string> typeArguments) =>
            genericType + "<" + string.Join(",", typeArguments) + ">";
        public string GetGenericMethodParameter(object? context, int index) => "!!" + index;
        public string GetGenericTypeParameter(object? context, int index) => "!" + index;
        public string GetFunctionPointerType(MethodSignature<string> signature) => "method-pointer";
        public string GetPrimitiveType(PrimitiveTypeCode typeCode) => "System." + typeCode;
    }
}
