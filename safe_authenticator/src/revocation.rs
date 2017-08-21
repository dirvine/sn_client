// Copyright 2017 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement.  This, along with the Licenses can be
// found in the root directory of this project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.
//
// Please review the Licences for the specific language governing permissions and limitations
// relating to use of the SAFE Network Software.

use super::{AccessContainerEntry, AuthError, AuthFuture};
use access_container::{self, AUTHENTICATOR_ENTRY};
use config::{self, AppInfo, RevocationQueue};
use futures::Future;
use futures::future::{self, Either, Loop};
use maidsafe_utilities::serialisation::{deserialise, serialise};
use routing::{EntryActions, User};
use rust_sodium::crypto::sign;
use safe_core::{Client, FutureExt, MDataInfo};
use safe_core::ipc::IpcError;
use safe_core::recovery;
use safe_core::utils::{symmetric_decrypt, symmetric_encrypt};
use std::collections::HashMap;

/// Revokes app access using a revocation queue
pub fn revoke_app(client: &Client<()>, app_id: &str) -> Box<AuthFuture<()>> {
    let app_id = app_id.to_string();
    let client = client.clone();
    let c2 = client.clone();

    config::get_app_revocation_queue(&client)
        .and_then(move |(version, queue)| {
            config::push_to_app_revocation_queue(
                &client,
                queue,
                config::next_version(version),
                app_id,
            )
        })
        .and_then(move |(version, queue)| {
            flush_app_revocation_queue_impl(&c2, queue, version + 1)
        })
        .into_box()
}

/// Revoke all apps currently in the revocation queue.
pub fn flush_app_revocation_queue(client: &Client<()>) -> Box<AuthFuture<()>> {
    let client = client.clone();

    config::get_app_revocation_queue(&client)
        .and_then(move |(version, queue)| if let Some(version) = version {
            flush_app_revocation_queue_impl(&client, queue, version + 1)
        } else {
            future::ok(()).into_box()
        })
        .into_box()
}

fn flush_app_revocation_queue_impl(
    client: &Client<()>,
    queue: RevocationQueue,
    version: u64,
) -> Box<AuthFuture<()>> {
    let client = client.clone();

    future::loop_fn((queue, version), move |(queue, version)| {
        let c2 = client.clone();
        let c3 = client.clone();

        if let Some(app_id) = queue.front().cloned() {
            let f = revoke_single_app(&c2, &app_id)
                .and_then(move |_| {
                    config::remove_from_app_revocation_queue(&c3, queue, version, app_id)
                })
                .and_then(move |(version, queue)| {
                    Ok(Loop::Continue((queue, version + 1)))
                });
            Either::A(f)
        } else {
            Either::B(future::ok(Loop::Break(())))
        }
    }).into_box()
}

/// Revoke access for a single app
fn revoke_single_app(client: &Client<()>, app_id: &str) -> Box<AuthFuture<()>> {
    let c2 = client.clone();
    let c4 = client.clone();
    let c5 = client.clone();
    let c6 = client.clone();
    let c7 = client.clone();
    let c8 = client.clone();

    // 1. Put the provided app_id into the revocation queue
    // 2. Delete the app key from MaidManagers
    // 3. Remove the app's key from containers permissions
    // 4. Refresh the containers info from the user's root dir (as the access
    //    container entry is not updated with the new keys info - so we have to
    //    make sure that we use correct encryption keys if the previous revoke\
    //    attempt has failed)
    // 4. Re-encrypt private containers that the app had access to
    // 5. Remove the revoked app from the access container
    config::get_app(client, app_id)
        .and_then(move |app| {
            delete_app_auth_key(&c2, app.keys.sign_pk).map(move |_| app)
        })
        .and_then(move |app| {
            access_container::fetch_entry(&c4, &app.info.id, app.keys.clone())
                .and_then(move |(version, permissions)| {
                    Ok((
                        app,
                        version,
                        permissions.ok_or(AuthError::IpcError(IpcError::UnknownApp))?,
                    ))
                })
        })
        .and_then(move |(app, ac_entry_version, permissions)| {
            revoke_container_perms(&c5, &permissions, app.keys.sign_pk)
                .map(move |_| (app, ac_entry_version, permissions))
        })
        .and_then(move |(app, ac_entry_version, permissions)| {
            refresh_from_access_container_root(&c6, permissions).map(move |refreshed_containers| {
                (app, ac_entry_version, refreshed_containers)
            })
        })
        .and_then(move |(app, ac_entry_version, permissions)| {
            reencrypt_private_containers(&c7, permissions.clone(), &app)
                .map(move |_| (app, ac_entry_version))
        })
        .and_then(move |(app, version)| {
            access_container::delete_entry(&c8, &app.info.id, &app.keys, version + 1)
        })
        .into_box()
}

/// Delete the app's auth key from the Maid Manager - this prevents the app from
/// performing any more mutations.
fn delete_app_auth_key(client: &Client<()>, key: sign::PublicKey) -> Box<AuthFuture<()>> {
    let client = client.clone();

    client
        .list_auth_keys_and_version()
        .and_then(move |(listed_keys, version)| if listed_keys.contains(
            &key,
        )
        {
            client.del_auth_key(key, version + 1)
        } else {
            // The key has been removed already
            ok!(())
        })
        .map_err(From::from)
        .into_box()
}

// Revokes containers permissions
fn revoke_container_perms(
    client: &Client<()>,
    permissions: &AccessContainerEntry,
    sign_pk: sign::PublicKey,
) -> Box<AuthFuture<()>> {
    let reqs: Vec<_> = permissions
        .values()
        .map(|&(ref mdata_info, _)| {
            let mdata_info = mdata_info.clone();
            let c2 = client.clone();

            client
                .clone()
                .get_mdata_version(mdata_info.name, mdata_info.type_tag)
                .and_then(move |version| {
                    recovery::del_mdata_user_permissions(
                        &c2,
                        mdata_info.name,
                        mdata_info.type_tag,
                        User::Key(sign_pk),
                        version + 1,
                    )
                })
                .map_err(From::from)
        })
        .collect();

    future::join_all(reqs).map(move |_| ()).into_box()
}

// Re-encrypts private containers for a revoked app
fn reencrypt_private_containers(
    client: &Client<()>,
    containers: AccessContainerEntry,
    revoked_app: &AppInfo,
) -> Box<AuthFuture<()>> {
    // 1. Make sure to get the latest containers info from the root dir (as it
    //    could have been updated on the previous failed revocation)
    // 2. Generate new encryption keys for all the containers to be reencrypted.
    // 3. Update the user root dir and the access container to use the new keys.
    // 4. Perform the actual reencryption of the containers.
    // 5. Update the user root dir and access container again, commiting or aborting
    //    the encryption keys change, depending on whether the re-encryption of the
    //    corresponding container succeeded or failed.
    let c2 = client.clone();
    let c3 = client.clone();

    let access_container = fry!(client.access_container().map_err(AuthError::from));

    let containers = start_new_containers_enc_info(containers);
    let app_key = fry!(access_container::enc_key(
        &access_container,
        &revoked_app.info.id,
        &revoked_app.keys.enc_key,
    ));

    update_access_container(
        client,
        access_container.clone(),
        containers.clone(),
        app_key.clone(),
    ).and_then(move |_| reencrypt_containers(&c2, containers))
        .and_then(move |containers| {
            update_access_container(&c3, access_container, containers, app_key).map(|_| ())
        })
        .into_box()
}

fn start_new_containers_enc_info(containers: AccessContainerEntry) -> Vec<(String, MDataInfo)> {
    containers
        .into_iter()
        .map(|(container, (mut mdata_info, _))| {
            if mdata_info.new_enc_info.is_none() {
                mdata_info.start_new_enc_info();
            }
            (container, mdata_info)
        })
        .collect()
}

/// Fetches containers info from the user's root dir
fn refresh_from_access_container_root(
    client: &Client<()>,
    containers: AccessContainerEntry,
) -> Box<AuthFuture<AccessContainerEntry>> {
    access_container::authenticator_entry(client)
        .and_then(move |(_, entries)| {
            Ok(
                containers
                    .into_iter()
                    .map(|(container, (mdata_info, perms))| {
                        if let Some(root_mdata_info) = entries.get(&container) {
                            (container, (root_mdata_info.clone(), perms))
                        } else {
                            (container, (mdata_info, perms))
                        }
                    })
                    .collect(),
            )
        })
        .map_err(AuthError::from)
        .into_box()
}

fn update_access_container(
    client: &Client<()>,
    access_container: MDataInfo,
    mut containers: Vec<(String, MDataInfo)>,
    revoked_app_key: Vec<u8>,
) -> Box<AuthFuture<()>> {
    let c2 = client.clone();
    let c3 = client.clone();

    let f_config = config::list_apps(client).map(|(_, apps)| apps);
    let f_entries = client
        .list_mdata_entries(access_container.name, access_container.type_tag)
        .map_err(From::from)
        .map(move |mut entries| {
            // Remove the revoked app entry from the access container
            // because we don't need it to be reencrypted.
            let _ = entries.remove(&revoked_app_key);
            entries
        });

    let auth_key = {
        let sk = fry!(client.secret_symmetric_key());
        fry!(access_container::enc_key(
            &access_container,
            AUTHENTICATOR_ENTRY,
            &sk,
        ))
    };

    f_config
        .join(f_entries)
        .and_then(move |(apps, entries)| {
            let mut actions = EntryActions::new();

            // Update the authenticator entry
            if let Some(raw) = entries.get(&auth_key) {
                let decoded = {
                    let sk = c2.secret_symmetric_key()?;
                    symmetric_decrypt(&raw.content, &sk)?
                };
                let mut decoded: HashMap<String, MDataInfo> = deserialise(&decoded)?;

                for &mut (ref container, ref mdata_info) in &mut containers {
                    if let Some(entry) = decoded.get_mut(container) {
                        *entry = mdata_info.clone();
                    }
                }

                let encoded = serialise(&decoded)?;
                let encoded = {
                    let sk = c2.secret_symmetric_key()?;
                    symmetric_encrypt(&encoded, &sk, None)?
                };

                actions = actions.update(auth_key, encoded, raw.entry_version + 1);
            }

            // Update apps' entries
            for app in apps.values() {
                let key =
                    access_container::enc_key(&access_container, &app.info.id, &app.keys.enc_key)?;

                if let Some(raw) = entries.get(&key) {
                    // Skip deleted entries.
                    if raw.content.is_empty() {
                        continue;
                    }

                    let decoded = symmetric_decrypt(&raw.content, &app.keys.enc_key)?;
                    let mut decoded: AccessContainerEntry = deserialise(&decoded)?;

                    for &mut (ref container, ref mdata_info) in &mut containers {
                        if let Some(entry) = decoded.get_mut(container) {
                            entry.0 = mdata_info.clone();
                        }
                    }

                    let encoded = serialise(&decoded)?;
                    let encoded = symmetric_encrypt(&encoded, &app.keys.enc_key, None)?;

                    actions = actions.update(key, encoded, raw.entry_version + 1);
                }
            }

            Ok((access_container, actions))
        })
        .and_then(move |(access_container, actions)| {
            recovery::mutate_mdata_entries(
                &c3,
                access_container.name,
                access_container.type_tag,
                actions.into(),
            ).map_err(From::from)
        })
        .into_box()
}

// Re-encrypt the given `containers` using the `new_enc_info` in the corresponding
// `MDataInfo`. Returns modified `containers` where the enc info regeneration is either
// commited or aborted, depending on if the re-encryption succeeded or failed.
fn reencrypt_containers(
    client: &Client<()>,
    containers: Vec<(String, MDataInfo)>,
) -> Box<AuthFuture<Vec<(String, MDataInfo)>>> {
    let c2 = client.clone();
    let fs = containers.into_iter().map(move |(container, mdata_info)| {
        let mut mdata_info2 = mdata_info.clone();
        let c3 = c2.clone();

        c2.list_mdata_entries(mdata_info.name, mdata_info.type_tag)
            .and_then(move |entries| {
                let mut actions = EntryActions::new();

                for (old_key, value) in entries {
                    // Skip deleted entries.
                    if value.content.is_empty() {
                        continue;
                    }

                    let plain_key = mdata_info.decrypt(&old_key)?;
                    let new_key = mdata_info.enc_entry_key(&plain_key)?;

                    let plain_content = mdata_info.decrypt(&value.content)?;
                    let new_content = mdata_info.enc_entry_value(&plain_content)?;

                    // Delete the old entry with the old key and
                    // insert the re-encrypted entry with a new key
                    actions = actions.del(old_key, value.entry_version + 1).ins(
                        new_key,
                        new_content,
                        0,
                    );
                }

                Ok((mdata_info, actions))
            })
            .and_then(move |(mdata_info, actions)| {
                recovery::mutate_mdata_entries(
                    &c3,
                    mdata_info.name,
                    mdata_info.type_tag,
                    actions.into(),
                ).map_err(From::from)
            })
            .then(move |res| {
                // If the mutation succeeded, commit the enc_info regeneration,
                // otherwise abort it.

                if res.is_ok() {
                    mdata_info2.commit_new_enc_info();
                } else {
                    // TODO: consider logging the error.
                    mdata_info2.abort_new_enc_info();
                }

                Ok((container, mdata_info2))
            })
    });

    future::join_all(fs).into_box()
}