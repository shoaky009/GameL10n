using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace VNTextPatch.Shared.Scripts.Yuris
{
    public sealed record YurisAttributeSource(int CommandIndex, string Command, int AttributeIndex,
        int AttributeId, string? Attribute, int AttributeType,
        [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Subroutine = null);

    public sealed record YurisTextSource(int GroupIndex, string Role, IReadOnlyList<YurisAttributeSource> Attributes);

    public readonly record struct YurisString(ScriptString String, YurisTextSource Source);
}
