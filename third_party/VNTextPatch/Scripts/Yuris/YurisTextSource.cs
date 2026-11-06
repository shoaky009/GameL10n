using System.Collections.Generic;

namespace VNTextPatch.Shared.Scripts.Yuris
{
    public sealed record YurisAttributeSource(int CommandIndex, string Command, int AttributeIndex,
        int AttributeId, string? Attribute, int AttributeType);

    public sealed record YurisTextSource(int GroupIndex, string Role, IReadOnlyList<YurisAttributeSource> Attributes);

    public readonly record struct YurisString(ScriptString String, YurisTextSource Source);
}
