namespace AvgTranslator.Core.Development;

/// <summary>A missing, damaged or inaccessible bundled parser asset, separate from format compatibility.</summary>
public sealed class BuiltinAdapterAssetException(string assetName, string code, string message, Exception? innerException = null)
    : IOException(message, innerException)
{
    public string AssetName { get; } = assetName;
    public string Code { get; } = code;
}
