using KeySharp;

namespace QsoRipper.Engine.DotNet;

internal enum QrzSecret
{
    XmlPassword,
    LogbookApiKey,
}

internal interface IQrzSecretStore
{
    string? Get(QrzSecret secret);

    void Set(QrzSecret secret, string value);
}

internal sealed class NullQrzSecretStore : IQrzSecretStore
{
    public string? Get(QrzSecret secret) => null;

    public void Set(QrzSecret secret, string value)
    {
    }
}

internal sealed class PlatformQrzSecretStore : IQrzSecretStore
{
    private const string Package = "com.treitforge.qsoripper";
    private const string Service = "qrz";

    public string? Get(QrzSecret secret)
    {
        try
        {
            return Keyring.GetPassword(Package, Service, GetAccount(secret));
        }
        catch (KeyringException exception) when (exception.Type == ErrorType.NotFound)
        {
            return null;
        }
        catch (Exception exception)
        {
            throw new QrzSecretStoreException("The platform credential store is unavailable.", exception);
        }
    }

    public void Set(QrzSecret secret, string value)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(value);

        try
        {
            Keyring.SetPassword(Package, Service, GetAccount(secret), value);
        }
        catch (Exception exception)
        {
            throw new QrzSecretStoreException("The platform credential store did not save the QRZ secret.", exception);
        }
    }

    private static string GetAccount(QrzSecret secret) =>
        secret switch
        {
            QrzSecret.XmlPassword => "xml-password",
            QrzSecret.LogbookApiKey => "logbook-api-key",
            _ => throw new ArgumentOutOfRangeException(nameof(secret)),
        };
}

internal sealed class QrzSecretStoreException : InvalidOperationException
{
    public QrzSecretStoreException()
    {
    }

    public QrzSecretStoreException(string message)
        : base(message)
    {
    }

    public QrzSecretStoreException(string message, Exception innerException)
        : base(message, innerException)
    {
    }
}
