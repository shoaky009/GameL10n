using System.Text;
using System.Text.Json;
using GameL10n.VnTextPatch;

if (args.Length > 0) return await AdvancedOperations.RunAsync(args);

Console.InputEncoding = new UTF8Encoding(false);
Console.OutputEncoding = new UTF8Encoding(false);
TextWriter protocolOutput = Console.Out;
Console.SetOut(Console.Error);
Request? request = null;
Response response;
try
{
    request = await JsonSerializer.DeserializeAsync<Request>(Console.OpenStandardInput(), VnTextPatchJson.TypeInfo<Request>(Worker.Json))
        ?? throw new InvalidDataException("请求内容为空。");
    response = Worker.Execute(request);
}
catch (Exception error)
{
    response = Worker.Failure(error);
}
if (request?.ResponseFile is { } responseFile)
{
    if (!Path.IsPathFullyQualified(responseFile)) throw new InvalidDataException("响应文件必须使用绝对路径。");
    await using var stream = File.Create(responseFile);
    await JsonSerializer.SerializeAsync(stream, response, VnTextPatchJson.TypeInfo<Response>(Worker.Json));
    await protocolOutput.WriteLineAsync(response.Success ? "ok" : "error");
}
else
{
    await protocolOutput.WriteLineAsync(JsonSerializer.Serialize(response, VnTextPatchJson.TypeInfo<Response>(Worker.Json)));
}
return response.Success ? 0 : 1;
