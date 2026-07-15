using QsoRipper.Engine.Lookup;
using QsoRipper.Engine.Lookup.Qrz;
using QsoRipper.Engine.QrzLogbook;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet;

internal interface IQrzCredentialTester
{
    Task<TestQrzCredentialsResponse> TestXmlCredentialsAsync(
        string username,
        string password,
        CancellationToken cancellationToken);

    Task<TestQrzLogbookCredentialsResponse> TestLogbookCredentialsAsync(
        string apiKey,
        CancellationToken cancellationToken);
}

internal sealed class QrzCredentialTester : IQrzCredentialTester
{
    public async Task<TestQrzCredentialsResponse> TestXmlCredentialsAsync(
        string username,
        string password,
        CancellationToken cancellationToken)
    {
        if (string.IsNullOrWhiteSpace(username) || string.IsNullOrWhiteSpace(password))
        {
            return new TestQrzCredentialsResponse
            {
                Success = false,
                ErrorMessage = "Username and password are required.",
            };
        }

        using var httpClient = new HttpClient();
        var provider = new QrzXmlProvider(httpClient, username.Trim(), password);
        var result = await provider.LookupAsync("W1AW", cancellationToken).ConfigureAwait(false);
        return result.State switch
        {
            ProviderLookupState.AuthenticationError or ProviderLookupState.SessionError =>
                new TestQrzCredentialsResponse
                {
                    Success = false,
                    ErrorMessage = result.ErrorMessage ?? "QRZ rejected the supplied XML credentials.",
                },
            ProviderLookupState.NetworkError => new TestQrzCredentialsResponse
            {
                Success = false,
                ErrorMessage = result.ErrorMessage ?? "QRZ could not be reached.",
            },
            _ => new TestQrzCredentialsResponse { Success = true },
        };
    }

    public async Task<TestQrzLogbookCredentialsResponse> TestLogbookCredentialsAsync(
        string apiKey,
        CancellationToken cancellationToken)
    {
        if (string.IsNullOrWhiteSpace(apiKey))
        {
            return new TestQrzLogbookCredentialsResponse
            {
                Success = false,
                ErrorMessage = "API key is required.",
            };
        }

        try
        {
            using var client = new QrzLogbookClient(apiKey.Trim());
            var status = await client.GetStatusAsync().WaitAsync(cancellationToken).ConfigureAwait(false);
            return new TestQrzLogbookCredentialsResponse
            {
                Success = true,
                LogbookOwner = status.Owner,
                QsoCount = status.QsoCount,
            };
        }
        catch (QrzLogbookException ex)
        {
            return new TestQrzLogbookCredentialsResponse
            {
                Success = false,
                ErrorMessage = ex.Message,
            };
        }
        catch (HttpRequestException ex)
        {
            return new TestQrzLogbookCredentialsResponse
            {
                Success = false,
                ErrorMessage = $"QRZ could not be reached: {ex.Message}",
            };
        }
    }
}
