Offline TLS fixtures for the proxy adapter tests only. The leaf private key is
public test data; it is never used by product code or installed in a trust store.
The test connector explicitly trusts root.pem. Its private key was discarded.

valid.pem covers checkip.amazonaws.com from 2020-01-01 to 2045-01-01.
wrong-host.pem has the same issuer but covers other.invalid. expired.pem expired
in 2021. Leaf PEMs contain one server certificate; root.pem is supplied only to the test connector. This also avoids the Windows PKCS#8 loader selecting the final certificate from a multi-certificate input. The installed UI
acceptance generates separate, short-lived certificates on its disposable CI
runner and removes that temporary trust in a finally block.
