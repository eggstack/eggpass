from eggpass_auth import (
    CancellationToken,
    Credentials,
    DeviceCode,
    OAuthClient,
    ProviderProfile,
    UserInfo,
)


def make_client() -> OAuthClient:
    profile: ProviderProfile = ProviderProfile.xai()
    return OAuthClient(profile, timeout_seconds=30, max_response_body_bytes=131072)


async def exercise_async_api(oauth: OAuthClient, device: DeviceCode, token: CancellationToken) -> None:
    credentials: Credentials = await oauth.poll_device_authorization(
        device, token, timeout_seconds=120
    )
    info: UserInfo = await oauth.userinfo(credentials.access_token)
    refreshed: Credentials = await oauth.refresh_credentials(credentials)
    assert info.user_id is None or isinstance(info.user_id, str)
    assert refreshed.refresh_token is None or isinstance(refreshed.refresh_token, str)
